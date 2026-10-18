//! Bundled-asset infrastructure: embedding, verified extraction, and safe
//! absolute paths for the native libraries and OCR models.
//!
//! chilli-pdf ships as a single executable with every native library
//! (PDFium, Tesseract) and traineddata file embedded in the binary. This
//! module extracts them into a **versioned per-user directory** and refuses
//! to hand out any file whose SHA-256 does not match the embedded manifest.
//!
//! Design rules (see `docs/architecture.md` § "Bundling & asset sandboxing"):
//!
//! * **Atomic extraction** — files are written to a sibling temp directory
//!   that is fully verified before an atomic rename publishes it. A crashed
//!   first launch can only leave a `.tmp-extract-*` directory behind, which
//!   is never treated as valid and is cleaned up on the next run.
//! * **Idempotent re-runs** — extracting over an existing, identical version
//!   skips the write and just re-verifies every checksum (cheap).
//! * **Version isolation** — `v1` and `v2` directories coexist; extraction
//!   never mutates or deletes another version's files.
//! * **Fail closed** — a corrupted or tampered asset is reported as an error
//!   and never loaded.
//! * **Absolute-path loading only** — [`ExtractedAssets::library_path`]
//!   returns absolute `PathBuf`s inside the verified directory so library
//!   loading never depends on `PATH`, the current directory, or any other
//!   system search path.
//!
//! The asset set lives in `assets/v1/` in the repository and is embedded via
//! `include_dir!`; `assets/manifest.txt` is the generated manifest
//! (`cargo run --example update_asset_manifest`). Swapping in the real
//! PDFium/Tesseract binaries later is therefore **data-only** — drop the
//! files into `assets/v1/`, regenerate the manifest, bump
//! [`ASSET_VERSION`] — with no code changes.

use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use dirs::home_dir;
use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};

/// Name of the per-user application directory under the OS data root.
pub const APP_DIR_NAME: &str = "chilli-pdf";

/// Version of the bundled asset set.
///
/// Bump this whenever any file in `assets/v1/` changes: extraction goes to
/// a fresh `v<n>` directory and old versions are left untouched (the user
/// may be running an older executable that still needs them).
pub const ASSET_VERSION: &str = "1";

/// The bundled asset set, embedded into the binary at compile time.
///
/// The real native libraries and traineddata files are not available yet;
/// `assets/v1/` currently holds byte-for-byte fixtures with the same names.
static BUNDLE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/v1");

/// The generated SHA-256 manifest of [`BUNDLE`].
const MANIFEST_TXT: &str = include_str!("../assets/manifest.txt");

/// Prefix for extraction temp directories (never a valid asset directory).
const TEMP_PREFIX: &str = ".tmp-extract-";

/// Everything that can go wrong while verifying or extracting assets.
#[derive(Debug)]
pub enum AssetError {
    /// The operating system did not provide a per-user data directory.
    UnknownUserDataDir,
    /// The embedded manifest is malformed.
    InvalidManifest(String),
    /// A manifest entry has no counterpart in the embedded bundle.
    MissingFromBundle(String),
    /// An expected asset file is missing on disk.
    MissingOnDisk(PathBuf),
    /// A file's content does not match the manifest (size or SHA-256).
    ChecksumMismatch {
        /// The file that failed verification.
        path: PathBuf,
        /// Expected SHA-256 from the manifest.
        expected: String,
        /// SHA-256 actually found.
        actual: String,
    },
    /// The target asset directory exists but is not a directory.
    NotADirectory(PathBuf),
    /// Filesystem failure during extraction or verification.
    Io {
        /// Operation context, e.g. `"rename extraction dir"`.
        op: &'static str,
        /// The path involved.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownUserDataDir => {
                write!(
                    f,
                    "the operating system provided no per-user data directory"
                )
            }
            Self::InvalidManifest(detail) => {
                write!(f, "the embedded asset manifest is invalid: {detail}")
            }
            Self::MissingFromBundle(name) => {
                write!(
                    f,
                    "asset `{name}` is listed in the manifest but missing from the embedded bundle"
                )
            }
            Self::MissingOnDisk(path) => {
                write!(f, "expected asset `{}` is missing", path.display())
            }
            Self::ChecksumMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "asset `{}` failed verification (expected SHA-256 {expected}, got {actual}); \
                 it may be corrupted or tampered with — refusing to load it",
                path.display()
            ),
            Self::NotADirectory(path) => {
                write!(
                    f,
                    "asset path `{}` exists but is not a directory",
                    path.display()
                )
            }
            Self::Io { op, path, source } => {
                write!(f, "{op} `{}` failed: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for AssetError {}

impl AssetError {
    fn io(op: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            op,
            path: path.into(),
            source,
        }
    }
}

/// Operating systems with distinct per-user data-directory conventions.
///
/// Used by the pure helpers so their path logic is testable on any host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `%LOCALAPPDATA%` under the user profile.
    Windows,
    /// `~/Library/Application Support`.
    Macos,
    /// `$XDG_DATA_HOME` (default `~/.local/share`).
    Linux,
}

/// The platform this binary was built for.
pub fn current_platform() -> Platform {
    if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Linux
    }
}

/// Per-user data root for `platform`, derived from the user's home
/// directory when no environment variable overrides it.
///
/// Pure function — the unit tests cover all three conventions on any host.
pub fn default_data_root(platform: Platform, home: &Path) -> PathBuf {
    match platform {
        Platform::Windows => home.join("AppData").join("Local"),
        Platform::Macos => home.join("Library").join("Application Support"),
        Platform::Linux => home.join(".local").join("share"),
    }
}

/// Per-user data root for `platform`, honoring each OS convention's
/// environment override exactly (see `docs/architecture.md`):
///
/// * **Windows** — `%LOCALAPPDATA%` when it holds an absolute path,
///   otherwise the `<home>\AppData\Local` default.
/// * **macOS** — `~/Library/Application Support`; the convention has no
///   environment override, so one is never consulted.
/// * **Linux** — `$XDG_DATA_HOME` when it holds an absolute path;
///   relative values are invalid per the XDG Base Directory
///   Specification and are ignored, otherwise `<home>/.local/share`.
///
/// Pure function: `env` is an injected lookup so every OS branch is
/// unit-testable on any host.
pub fn data_root_for(
    platform: Platform,
    home: &Path,
    env: impl Fn(&str) -> Option<PathBuf>,
) -> PathBuf {
    let env_override = match platform {
        Platform::Windows => env("LOCALAPPDATA").filter(|path| path.is_absolute()),
        Platform::Macos => None,
        Platform::Linux => env("XDG_DATA_HOME").filter(|path| path.is_absolute()),
    };
    env_override.unwrap_or_else(|| default_data_root(platform, home))
}

/// The per-user application directory (`<data root>/chilli-pdf`) for an
/// explicit platform, home directory, and environment lookup.
///
/// Pure function — this is exactly how [`user_asset_base_dir`] resolves
/// the host's directory, hoisted so the composition is unit-testable
/// per OS on any host.
pub fn user_asset_base_dir_from(
    platform: Platform,
    home: &Path,
    env: impl Fn(&str) -> Option<PathBuf>,
) -> PathBuf {
    asset_base_dir(&data_root_for(platform, home, env))
}

/// The per-user base directory (`<data root>/chilli-pdf`) on this OS.
///
/// Resolution follows the OS conventions exactly (see [`data_root_for`]):
/// `%LOCALAPPDATA%` on Windows so extraction never roams with the user,
/// `~/Library/Application Support` on macOS, and `$XDG_DATA_HOME`
/// (default `~/.local/share`) on Linux. Only the home directory comes
/// from the `dirs` crate; the rest is pure, unit-tested logic.
pub fn user_asset_base_dir() -> Result<PathBuf, AssetError> {
    let home = home_dir().ok_or(AssetError::UnknownUserDataDir)?;
    Ok(user_asset_base_dir_from(current_platform(), &home, |key| {
        env::var_os(key).map(PathBuf::from)
    }))
}

/// Application directory under an OS-provided per-user data root:
/// `<root>/chilli-pdf`.
pub fn asset_base_dir(data_root: &Path) -> PathBuf {
    data_root.join(APP_DIR_NAME)
}

/// Versioned asset directory: `<base>/assets/v<version>/`.
pub fn versioned_dir(base: &Path, version: &str) -> PathBuf {
    base.join("assets").join(format!("v{version}"))
}

/// A single manifest entry: relative path, byte size, and SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// Path relative to the asset directory (e.g. `tessdata/eng.traineddata`).
    pub name: String,
    /// Exact file size in bytes.
    pub size: u64,
    /// SHA-256 of the file content.
    pub sha256: [u8; 32],
}

/// Parsed form of the embedded manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetManifest {
    entries: Vec<AssetEntry>,
}

impl AssetManifest {
    /// Parse the manifest text format:
    ///
    /// ```text
    /// # comment lines
    /// <sha256-hex> <size-bytes> <relative-path>
    /// ```
    pub fn parse(text: &str) -> Result<Self, AssetError> {
        let mut entries = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = || AssetError::InvalidManifest(format!("line {}: `{line}`", index + 1));
            let (hash, rest) = line
                .split_once(' ')
                .filter(|(_, r)| !r.starts_with(' '))
                .ok_or_else(bad)?;
            let (size, name) = rest.split_once(' ').ok_or_else(bad)?;
            if name.contains(' ') || name.starts_with('/') || name.contains("..") {
                return Err(bad());
            }
            if hash.len() != 64 {
                return Err(bad());
            }
            let mut sha256 = [0u8; 32];
            hex::decode_to_slice(hash, &mut sha256).map_err(|_| bad())?;
            let size: u64 = size.parse().map_err(|_| bad())?;
            if size == 0 {
                return Err(bad());
            }
            entries.push(AssetEntry {
                name: name.to_owned(),
                size,
                sha256,
            });
        }
        if entries.is_empty() {
            return Err(AssetError::InvalidManifest("no asset entries".to_owned()));
        }
        Ok(Self { entries })
    }

    /// Manifest entries in file order.
    pub fn entries(&self) -> &[AssetEntry] {
        &self.entries
    }

    /// Look an entry up by relative path.
    pub fn entry(&self, name: &str) -> Option<&AssetEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

/// The parsed embedded manifest (computed once).
pub fn manifest() -> &'static AssetManifest {
    static MANIFEST: OnceLock<AssetManifest> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        AssetManifest::parse(MANIFEST_TXT).unwrap_or_else(|e| {
            panic!("bundled asset manifest must parse (regenerate it with `cargo run --example update_asset_manifest`): {e}")
        })
    })
}

/// Verify a byte buffer against the embedded manifest.
///
/// `name` is the path relative to the asset directory. Fails closed on
/// unknown names, size mismatches, and checksum mismatches.
pub fn verify_bytes(name: &str, bytes: &[u8]) -> Result<(), AssetError> {
    let entry = manifest()
        .entry(name)
        .ok_or_else(|| AssetError::InvalidManifest(format!("`{name}` is not in the manifest")))?;
    if bytes.len() as u64 != entry.size {
        return Err(AssetError::ChecksumMismatch {
            path: PathBuf::from(name),
            expected: hex::encode(entry.sha256),
            actual: format!("size {} bytes", bytes.len()),
        });
    }
    let actual = Sha256::digest(bytes);
    if actual[..] != entry.sha256 {
        return Err(AssetError::ChecksumMismatch {
            path: PathBuf::from(name),
            expected: hex::encode(entry.sha256),
            actual: hex::encode(actual),
        });
    }
    Ok(())
}

/// Verify a file on disk against the embedded manifest.
fn verify_file(path: &Path, entry: &AssetEntry) -> Result<(), AssetError> {
    let bytes = fs::read(path).map_err(|e| AssetError::io("read asset", path, e))?;
    let actual = Sha256::digest(&bytes);
    if actual[..] != entry.sha256 || bytes.len() as u64 != entry.size {
        return Err(AssetError::ChecksumMismatch {
            path: path.to_path_buf(),
            expected: hex::encode(entry.sha256),
            actual: hex::encode(actual),
        });
    }
    Ok(())
}

/// Verify every manifest-listed file in `dir` against the embedded
/// manifest.
///
/// Missing files and checksum mismatches are hard errors: callers must
/// fail closed and never hand out an unverifiable path.
pub fn verify_dir(dir: &Path) -> Result<(), AssetError> {
    for entry in manifest().entries() {
        let path = dir.join(&entry.name);
        if !path.is_file() {
            return Err(AssetError::MissingOnDisk(path));
        }
        verify_file(&path, entry)?;
    }
    Ok(())
}

/// Verify the embedded bundle against the manifest.
///
/// This is the build-time staleness check: it fails when
/// `assets/manifest.txt` does not exactly describe `assets/v1/`
/// (regenerate it with `cargo run --example update_asset_manifest`).
pub fn verify_embedded_bundle() -> Result<(), AssetError> {
    let mut bundle_names: Vec<String> = Vec::new();
    collect_bundle_names(&BUNDLE, &mut bundle_names);
    bundle_names.sort();
    for name in &bundle_names {
        let file = BUNDLE
            .get_file(name)
            .expect("collected from the bundle itself");
        verify_bytes(name, file.contents())?;
    }
    for entry in manifest().entries() {
        if !bundle_names.contains(&entry.name) {
            return Err(AssetError::MissingFromBundle(entry.name.clone()));
        }
    }
    Ok(())
}

/// Recursively collect file paths relative to `dir`.
fn collect_bundle_names(dir: &Dir<'_>, out: &mut Vec<String>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(sub) => collect_bundle_names(sub, out),
            include_dir::DirEntry::File(file) => {
                let name = file
                    .path()
                    .to_string_lossy()
                    .trim_start_matches('/')
                    .to_owned();
                out.push(name);
            }
        }
    }
}

/// A verified, extracted asset directory.
///
/// Paths obtained from this type are always absolute and always point
/// inside a directory whose every file passed manifest verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAssets {
    dir: PathBuf,
}

impl ExtractedAssets {
    /// The extracted asset directory (`<base>/assets/v<version>`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Absolute path of a native library inside the verified directory.
    ///
    /// Callers must load libraries **only** from this absolute path — never
    /// via `PATH` or current-directory lookups — so a tampered library in a
    /// search path can never be picked up instead of the verified one.
    /// On Windows the eventual loader must additionally use
    /// `LoadLibraryExW` with `LOAD_LIBRARY_SEARCH_*` flags (see
    /// `docs/architecture.md`).
    pub fn library_path(&self, library: NativeLib) -> Result<PathBuf, AssetError> {
        let path = self
            .dir
            .join(library.file_name_for(current_platform()))
            .canonicalize()
            .map_err(|e| AssetError::io("resolve library path", &self.dir, e))?;
        Ok(path)
    }

    /// Absolute path of a bundled traineddata file (e.g. `eng`).
    pub fn traineddata_path(&self, code: &str) -> Result<PathBuf, AssetError> {
        let name = format!("tessdata/{code}.traineddata");
        let entry = manifest().entry(&name).ok_or_else(|| {
            AssetError::InvalidManifest(format!("`{name}` is not in the manifest"))
        })?;
        let path = self.dir.join(&name);
        if !path.is_file() {
            return Err(AssetError::MissingOnDisk(path));
        }
        // `entry` is used by the manifest lookup contract; verify too.
        verify_file(&path, entry)?;
        Ok(path)
    }
}

/// Native libraries bundled with chilli-pdf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeLib {
    /// PDFium — PDF rendering and page manipulation.
    Pdfium,
    /// Tesseract 5 — optical character recognition.
    Tesseract,
}

impl NativeLib {
    /// Platform file name of a library, e.g. `pdfium.dll`.
    pub fn file_name_for(self, platform: Platform) -> &'static str {
        match (self, platform) {
            (Self::Pdfium, Platform::Windows) => "pdfium.dll",
            (Self::Pdfium, Platform::Macos) => "libpdfium.dylib",
            (Self::Pdfium, Platform::Linux) => "libpdfium.so",
            (Self::Tesseract, Platform::Windows) => "tesseract.dll",
            (Self::Tesseract, Platform::Macos) => "libtesseract.dylib",
            (Self::Tesseract, Platform::Linux) => "libtesseract.so",
        }
    }

    /// Platform file name of the library on the current OS.
    pub fn platform_file_name(self) -> &'static str {
        self.file_name_for(current_platform())
    }
}

/// Ensure the asset set `version` is extracted under `base` and verified.
///
/// * If `<base>/assets/v<version>/` already exists, its files are only
///   re-verified against the manifest (cheap, idempotent). Any mismatch is
///   an error — chilli-pdf fails closed instead of quietly re-extracting,
///   because a corrupt *published* directory means disk corruption or
///   tampering, and the user must hear about it.
/// * Otherwise the bundle is written to a sibling temp directory, fully
///   verified, and atomically renamed into place. An interrupted run can
///   never publish a half-written directory.
///
/// Other versions' directories are never touched.
pub fn ensure_extracted(base: &Path, version: &str) -> Result<ExtractedAssets, AssetError> {
    let final_dir = versioned_dir(base, version);
    if final_dir.exists() {
        if !final_dir.is_dir() {
            return Err(AssetError::NotADirectory(final_dir));
        }
        verify_dir(&final_dir)?;
        return Ok(ExtractedAssets { dir: final_dir });
    }

    let assets_root = base.join("assets");
    fs::create_dir_all(&assets_root)
        .map_err(|e| AssetError::io("create assets root", &assets_root, e))?;
    remove_stale_temp_dirs(&assets_root, version)?;

    let temp_dir = extract_to_temp(&assets_root, version)?;
    match fs::rename(&temp_dir, &final_dir) {
        Ok(()) => {}
        Err(_) if final_dir.is_dir() => {
            // Lost a race against another extraction of the same version:
            // clean up our own temp copy and accept the published one.
            let _ = fs::remove_dir_all(&temp_dir);
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(AssetError::io("publish extraction dir", &final_dir, e));
        }
    }

    verify_dir(&final_dir)?;
    Ok(ExtractedAssets { dir: final_dir })
}

/// Write the whole bundle into a fresh temp directory, verify it, and
/// return the temp path — **without** publishing it.
///
/// Split out so the interrupted-extraction invariant is testable: nothing
/// outside the temp directory is touched, and the published directory is
/// only ever created by the atomic rename in [`ensure_extracted`].
fn extract_to_temp(assets_root: &Path, version: &str) -> Result<PathBuf, AssetError> {
    let temp_dir = assets_root.join(format!("{TEMP_PREFIX}v{version}-{}", std::process::id()));
    fs::create_dir_all(&temp_dir)
        .map_err(|e| AssetError::io("create extraction temp dir", &temp_dir, e))?;

    match write_bundle(&temp_dir).and_then(|()| verify_dir(&temp_dir)) {
        Ok(()) => Ok(temp_dir),
        Err(e) => {
            // Never leave a half-written temp directory behind.
            let _ = fs::remove_dir_all(&temp_dir);
            Err(e)
        }
    }
}

/// Write every manifest-listed embedded file under `dir`, creating parent
/// directories as needed and syncing each file to disk.
fn write_bundle(dir: &Path) -> Result<(), AssetError> {
    for entry in manifest().entries() {
        let file = BUNDLE
            .get_file(&entry.name)
            .ok_or_else(|| AssetError::MissingFromBundle(entry.name.clone()))?;
        let path = dir.join(&entry.name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| AssetError::io("create asset parent dir", parent, e))?;
        }
        // Write and fsync through the SAME write-capable handle. On Windows,
        // FlushFileBuffers requires write access to the file, so syncing a
        // second, read-only handle fails with Access is denied (error 5).
        let mut out =
            fs::File::create(&path).map_err(|e| AssetError::io("write asset", &path, e))?;
        out.write_all(file.contents())
            .map_err(|e| AssetError::io("write asset", &path, e))?;
        out.sync_all()
            .map_err(|e| AssetError::io("sync asset", &path, e))?;
    }
    Ok(())
}

/// Best-effort removal of leftover temp directories from interrupted
/// extractions of `version`. Only directories matching the version's temp
/// prefix are considered; other versions' temp dirs are left alone.
fn remove_stale_temp_dirs(assets_root: &Path, version: &str) -> Result<(), AssetError> {
    let prefix = format!("{TEMP_PREFIX}v{version}-");
    let read = match fs::read_dir(assets_root) {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(AssetError::io("read assets root", assets_root, e)),
    };
    for entry in read {
        let entry = entry.map_err(|e| AssetError::io("read assets root entry", assets_root, e))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok(())
}

/// Extract-if-needed and verify the bundled assets of this build.
///
/// App-facing entry point: resolves the per-user directory for the current
/// OS and ensures [`ASSET_VERSION`] is extracted there.
pub fn ensure_assets() -> Result<ExtractedAssets, AssetError> {
    let base = user_asset_base_dir()?;
    ensure_extracted(&base, ASSET_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Fresh scratch base directory per test (unique name avoids the
    /// shared `cargo test` thread pool racing on one path).
    fn scratch_base(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chilli-pdf-assets-tests-{}-{label}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn extracted_assets(base: &Path, version: &str) -> ExtractedAssets {
        ensure_extracted(base, version).unwrap()
    }

    // ---- manifest ----

    #[test]
    fn embedded_manifest_parses_and_lists_every_expected_asset() {
        let manifest = manifest();
        let names: Vec<&str> = manifest.entries().iter().map(|e| e.name.as_str()).collect();
        for expected in [
            "pdfium.dll",
            "libpdfium.dylib",
            "libpdfium.so",
            "tesseract.dll",
            "libtesseract.dylib",
            "libtesseract.so",
            "tessdata/eng.traineddata",
            "tessdata/hun.traineddata",
        ] {
            assert!(
                names.contains(&expected),
                "missing `{expected}` in {names:?}"
            );
        }
    }

    #[test]
    fn manifest_rejects_corrupted_lines() {
        assert!(AssetManifest::parse("not a manifest").is_err());
        assert!(AssetManifest::parse("").is_err());
        assert!(AssetManifest::parse("# comment only\n").is_err());
        // Bad hex.
        assert!(AssetManifest::parse("zz 5 a.dll").is_err());
        // Wrong size field.
        assert!(
            AssetManifest::parse(
                "1b953e823a90348a5aa1a2cf4f976c93399e2cad0791f6c65864f3c9cb75c6ab twelve a.dll"
            )
            .is_err()
        );
        // Missing name.
        assert!(
            AssetManifest::parse(
                "1b953e823a90348a5aa1a2cf4f976c93399e2cad0791f6c65864f3c9cb75c6ab 5"
            )
            .is_err()
        );
        // Absolute or parent-escaping paths are rejected.
        assert!(
            AssetManifest::parse(
                "1b953e823a90348a5aa1a2cf4f976c93399e2cad0791f6c65864f3c9cb75c6ab 5 /etc/passwd"
            )
            .is_err()
        );
        assert!(
            AssetManifest::parse(
                "1b953e823a90348a5aa1a2cf4f976c93399e2cad0791f6c65864f3c9cb75c6ab 5 ../escape.dll"
            )
            .is_err()
        );
    }

    #[test]
    fn valid_manifest_lines_round_trip() {
        let text = concat!(
            "# comment\n",
            "1b953e823a90348a5aa1a2cf4f976c93399e2cad0791f6c65864f3c9cb75c6ab 1140 libpdfium.dylib\n",
            "\n",
        );
        let manifest = AssetManifest::parse(text).unwrap();
        assert_eq!(manifest.entries().len(), 1);
        let entry = manifest.entry("libpdfium.dylib").unwrap();
        assert_eq!(entry.size, 1140);
        assert_eq!(entry.sha256.len(), 32);
    }

    // ---- verification ----

    #[test]
    fn embedded_bundle_matches_manifest() {
        verify_embedded_bundle().unwrap();
    }

    #[test]
    fn verify_bytes_accepts_pristine_asset_and_rejects_corrupted_one() {
        let file = BUNDLE.get_file("tessdata/eng.traineddata").unwrap();
        verify_bytes("tessdata/eng.traineddata", file.contents()).unwrap();

        let mut corrupted = file.contents().to_vec();
        corrupted[10] ^= 0xff;
        assert!(matches!(
            verify_bytes("tessdata/eng.traineddata", &corrupted),
            Err(AssetError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn verify_bytes_rejects_truncated_asset() {
        let file = BUNDLE.get_file("pdfium.dll").unwrap();
        assert!(matches!(
            verify_bytes("pdfium.dll", &file.contents()[..10]),
            Err(AssetError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn verify_bytes_rejects_unknown_name() {
        assert!(verify_bytes("not-an-asset.bin", b"payload").is_err());
    }

    #[test]
    fn verify_dir_detects_corrupted_checksum() {
        let base = scratch_base("verify-corrupt");
        let assets = extracted_assets(&base, "1");
        let victim = assets.dir().join("tessdata/hun.traineddata");
        let original = fs::read(&victim).unwrap();
        fs::write(&victim, b"tampered bytes").unwrap();
        assert!(matches!(
            verify_dir(assets.dir()),
            Err(AssetError::ChecksumMismatch { .. })
        ));
        // Fail closed: ensure_extracted must refuse the corrupt directory.
        assert!(ensure_extracted(&base, "1").is_err());
        fs::write(&victim, original).unwrap();
        verify_dir(assets.dir()).unwrap();
    }

    #[test]
    fn verify_dir_detects_missing_file() {
        let base = scratch_base("verify-missing");
        let assets = extracted_assets(&base, "1");
        fs::remove_file(assets.dir().join("tesseract.dll")).unwrap();
        assert!(matches!(
            verify_dir(assets.dir()),
            Err(AssetError::MissingOnDisk(_))
        ));
    }

    // ---- extraction: atomicity & idempotency ----

    #[test]
    fn extraction_creates_versioned_layout_with_all_files() {
        let base = scratch_base("layout");
        let assets = extracted_assets(&base, "1");
        assert_eq!(assets.dir(), base.join("assets").join("v1"));
        verify_dir(assets.dir()).unwrap();
        // Spot-check that real content arrived, not just empty files.
        let dll = fs::read(assets.dir().join("pdfium.dll")).unwrap();
        assert!(dll.starts_with(b"MZ chilli-pdf fixture"));
    }

    #[test]
    fn extraction_is_idempotent_over_existing_identical_version() {
        let base = scratch_base("idempotent");
        let first = extracted_assets(&base, "1");
        // A foreign file we just added must survive a re-run: the cheap
        // path must not wipe and re-extract the directory.
        let marker = first.dir().join("user-created-marker.txt");
        fs::write(&marker, "untouched").unwrap();
        let second = ensure_extracted(&base, "1").unwrap();
        assert_eq!(first.dir(), second.dir());
        assert_eq!(fs::read_to_string(&marker).unwrap(), "untouched");
        verify_dir(second.dir()).unwrap();
    }

    #[test]
    fn interrupted_extraction_never_publishes_a_partial_directory() {
        let base = scratch_base("interrupted");
        let assets_root = base.join("assets");
        // Simulate a crash after writing but before the atomic rename: the
        // low-level step leaves a fully-written (or partial) temp dir and
        // the published directory does not exist.
        let leftover = extract_to_temp(&assets_root, "1").unwrap();
        assert!(leftover.starts_with(&assets_root));
        let final_dir = versioned_dir(&base, "1");
        assert!(
            !final_dir.exists(),
            "temp must not be published by extract_to_temp"
        );

        // Recovery: the next run ignores the leftover, extracts cleanly,
        // publishes atomically, and removes the stale temp dir.
        let assets = extracted_assets(&base, "1");
        assert_eq!(assets.dir(), final_dir);
        verify_dir(assets.dir()).unwrap();
        assert!(!leftover.exists(), "stale temp dir must be cleaned up");
    }

    #[test]
    fn interrupted_extraction_with_garbage_temp_dir_is_recovered() {
        let base = scratch_base("garbage-temp");
        let assets_root = base.join("assets");
        fs::create_dir_all(&assets_root).unwrap();
        // A crashed run from a "different process" left a partial temp dir
        // behind, half full of garbage.
        let stale = assets_root.join(".tmp-extract-v1-999999");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("pdfium.dll"), b"half-written garbage").unwrap();

        let assets = extracted_assets(&base, "1");
        verify_dir(assets.dir()).unwrap();
        assert!(!stale.exists(), "stale temp dir must be cleaned up");
    }

    #[test]
    fn unwritable_assets_root_fails_closed_and_publishes_nothing() {
        let base = scratch_base("blocked-root");
        // A file squatting on the assets root makes every write fail.
        fs::write(base.join("assets"), b"file where the assets root should be").unwrap();
        let final_dir = versioned_dir(&base, "1");
        assert!(ensure_extracted(&base, "1").is_err());
        assert!(
            !final_dir.exists(),
            "a failed run must never publish assets"
        );
    }

    #[test]
    fn mid_bundle_write_failure_leaves_nothing_usable() {
        // Pre-plant a file where the tessdata subdirectory must go, so the
        // write step fails partway through the bundle.
        let base = scratch_base("write-failure");
        fs::write(base.join("tessdata"), b"blocking file").unwrap();
        assert!(write_bundle(&base).is_err());
        // Callers only ever publish through the atomic rename in
        // ensure_extracted, so a partial write like this never becomes
        // an asset directory the app would treat as valid.
        let final_dir = versioned_dir(&base, "1");
        assert!(!final_dir.exists());
    }

    #[test]
    fn existing_non_directory_in_the_way_fails_closed() {
        let base = scratch_base("not-a-dir");
        let final_dir = versioned_dir(&base, "1");
        fs::create_dir_all(final_dir.parent().unwrap()).unwrap();
        fs::write(&final_dir, b"a file where the dir should be").unwrap();
        assert!(matches!(
            ensure_extracted(&base, "1"),
            Err(AssetError::NotADirectory(_))
        ));
    }

    // ---- version isolation ----

    #[test]
    fn versions_coexist_and_never_touch_each_other() {
        let base = scratch_base("versions");
        let v1 = extracted_assets(&base, "1");
        let marker = v1.dir().join("user-created-marker.txt");
        fs::write(&marker, "v1 property").unwrap();

        let v2 = extracted_assets(&base, "2");
        assert_ne!(v1.dir(), v2.dir());
        assert_eq!(v2.dir(), base.join("assets").join("v2"));

        // Extracting v2 neither damaged nor removed v1.
        verify_dir(v1.dir()).unwrap();
        assert_eq!(fs::read_to_string(&marker).unwrap(), "v1 property");
        verify_dir(v2.dir()).unwrap();
    }

    // ---- per-OS path selection (pure functions) ----

    #[test]
    fn per_os_data_roots_follow_platform_conventions() {
        let home = Path::new("/users/anna");
        assert_eq!(
            default_data_root(Platform::Windows, home),
            PathBuf::from("/users/anna/AppData/Local")
        );
        assert_eq!(
            default_data_root(Platform::Macos, home),
            PathBuf::from("/users/anna/Library/Application Support")
        );
        assert_eq!(
            default_data_root(Platform::Linux, home),
            PathBuf::from("/users/anna/.local/share")
        );
    }

    #[test]
    fn base_and_versioned_dirs_compose_the_documented_layout() {
        let root = Path::new("/data-root");
        let base = asset_base_dir(root);
        assert_eq!(base, PathBuf::from("/data-root/chilli-pdf"));
        assert_eq!(
            versioned_dir(&base, "1"),
            PathBuf::from("/data-root/chilli-pdf/assets/v1")
        );
        assert_eq!(
            versioned_dir(&base, "42"),
            PathBuf::from("/data-root/chilli-pdf/assets/v42")
        );
    }

    // ---- per-OS data-root resolution (pure functions) ----

    /// An environment-provided root that is absolute on whichever host
    /// runs the tests: Windows drive paths are not absolute under Unix
    /// and vice versa, and the overrides only accept absolute values.
    fn absolute_env_root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"D:\CustomDataRoot")
        } else {
            PathBuf::from("/custom-data-root")
        }
    }

    #[test]
    fn windows_data_root_uses_localappdata_when_it_is_absolute() {
        let home = Path::new("/users/anna");
        let custom = absolute_env_root();
        let env = |_: &str| Some(custom.clone());
        assert_eq!(data_root_for(Platform::Windows, home, env), custom);
    }

    #[test]
    fn windows_data_root_ignores_missing_or_relative_localappdata() {
        let home = Path::new("/users/anna");
        let expected = PathBuf::from("/users/anna/AppData/Local");
        let missing = |_: &str| None;
        assert_eq!(data_root_for(Platform::Windows, home, missing), expected);
        let relative = |_: &str| Some(PathBuf::from("relative/junk"));
        assert_eq!(data_root_for(Platform::Windows, home, relative), expected);
    }

    #[test]
    fn linux_data_root_uses_xdg_data_home_when_it_is_absolute() {
        let home = Path::new("/users/anna");
        let custom = absolute_env_root();
        let env = |_: &str| Some(custom.clone());
        assert_eq!(data_root_for(Platform::Linux, home, env), custom);
    }

    #[test]
    fn linux_data_root_ignores_missing_or_relative_xdg_data_home() {
        // A relative $XDG_DATA_HOME is invalid per the XDG Base Directory
        // Specification; the default must be used instead.
        let home = Path::new("/users/anna");
        let expected = PathBuf::from("/users/anna/.local/share");
        let missing = |_: &str| None;
        assert_eq!(data_root_for(Platform::Linux, home, missing), expected);
        let relative = |_: &str| Some(PathBuf::from("relative/junk"));
        assert_eq!(data_root_for(Platform::Linux, home, relative), expected);
    }

    #[test]
    fn macos_data_root_has_no_environment_override() {
        // The environment lookup always yields a value and macOS must
        // still ignore it: the convention has no override.
        let home = Path::new("/users/anna");
        let env = |_: &str| Some(absolute_env_root());
        assert_eq!(
            data_root_for(Platform::Macos, home, env),
            PathBuf::from("/users/anna/Library/Application Support")
        );
    }

    #[test]
    fn user_asset_base_dir_from_composes_root_and_app_dir_for_every_platform() {
        let home = Path::new("/users/anna");
        let no_env = |_: &str| None;
        for (platform, root) in [
            (Platform::Windows, "/users/anna/AppData/Local"),
            (Platform::Macos, "/users/anna/Library/Application Support"),
            (Platform::Linux, "/users/anna/.local/share"),
        ] {
            let base = user_asset_base_dir_from(platform, home, no_env);
            assert_eq!(base, PathBuf::from(format!("{root}/{APP_DIR_NAME}")));
            assert!(base.ends_with(APP_DIR_NAME));
        }
    }

    #[test]
    fn user_asset_base_dir_on_this_os_points_at_chilli_pdf() {
        let base = user_asset_base_dir().unwrap();
        assert!(base.ends_with(APP_DIR_NAME), "got {}", base.display());
    }

    #[test]
    fn ensure_assets_extracts_into_the_real_user_directory() {
        // Uses the actual per-user directory of the test account (a
        // scratch version avoids touching any real extraction).
        let base = user_asset_base_dir().unwrap();
        let assets = ensure_extracted(&base, "test-ensure-assets").unwrap();
        assert_eq!(assets.dir(), versioned_dir(&base, "test-ensure-assets"));
        verify_dir(assets.dir()).unwrap();
        fs::remove_dir_all(assets.dir()).unwrap();
    }

    // ---- native-library path helpers ----

    #[test]
    fn native_library_file_names_follow_platform_conventions() {
        assert_eq!(
            NativeLib::Pdfium.file_name_for(Platform::Windows),
            "pdfium.dll"
        );
        assert_eq!(
            NativeLib::Pdfium.file_name_for(Platform::Macos),
            "libpdfium.dylib"
        );
        assert_eq!(
            NativeLib::Pdfium.file_name_for(Platform::Linux),
            "libpdfium.so"
        );
        assert_eq!(
            NativeLib::Tesseract.file_name_for(Platform::Windows),
            "tesseract.dll"
        );
        assert_eq!(
            NativeLib::Tesseract.file_name_for(Platform::Macos),
            "libtesseract.dylib"
        );
        assert_eq!(
            NativeLib::Tesseract.file_name_for(Platform::Linux),
            "libtesseract.so"
        );
    }

    #[test]
    fn library_paths_are_absolute_and_verified() {
        let base = scratch_base("library-paths");
        let assets = extracted_assets(&base, "1");
        let canonical_dir = assets.dir().canonicalize().unwrap();
        for lib in [NativeLib::Pdfium, NativeLib::Tesseract] {
            let path = assets.library_path(lib).unwrap();
            assert!(path.is_absolute(), "not absolute: {}", path.display());
            assert!(path.is_file(), "missing on disk: {}", path.display());
            assert_eq!(path, canonical_dir.join(lib.platform_file_name()));
            verify_dir(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn traineddata_paths_cover_all_presets_and_verify() {
        let base = scratch_base("tessdata");
        let assets = extracted_assets(&base, "1");
        for code in ["eng", "hun"] {
            let path = assets.traineddata_path(code).unwrap();
            assert!(path.is_absolute());
            assert!(path.is_file());
            assert!(path.ends_with(format!("{code}.traineddata")));
        }
        assert!(assets.traineddata_path("ita").is_err());
    }
}
