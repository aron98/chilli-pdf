//! Locating the native OCR runtime: library and traineddata.
//!
//! Production supplies the extracted assets (issue #9 / PR #24); development
//! uses a Tesseract installed by the system package manager via
//! [`SystemOcrPaths`]. Both speak the same [`OcrPaths`] trait, so the engine
//! never depends on a particular delivery mechanism.

use std::env;
#[cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};

use super::error::OcrError;

/// Where the OCR engine finds its native library and traineddata.
///
/// The asset layer (PR #24) implements this against its extracted directory;
/// [`SystemOcrPaths`] implements it against a package-manager install.
pub trait OcrPaths {
    /// Native libraries to load, dependencies first (e.g. libleptonica before
    /// libtesseract). The last entry must be libtesseract itself.
    fn library_paths(&self) -> Result<Vec<PathBuf>, OcrError>;
    /// Directory holding the `*.traineddata` files for the requested language.
    fn tessdata_dir(&self) -> Result<PathBuf, OcrError>;
}

/// Caller-provided absolute paths.
///
/// Use this for tests, custom installs, and as the adapter the asset layer
/// (PR #24) wraps: it forwards the extracted assets' library and traineddata
/// paths into the two trait methods.
#[derive(Debug, Clone)]
pub struct StaticOcrPaths {
    libraries: Vec<PathBuf>,
    tessdata_dir: PathBuf,
}

impl StaticOcrPaths {
    /// Build paths from an explicit library list and tessdata directory.
    pub fn new(libraries: Vec<PathBuf>, tessdata_dir: impl Into<PathBuf>) -> Self {
        Self {
            libraries,
            tessdata_dir: tessdata_dir.into(),
        }
    }
}

impl OcrPaths for StaticOcrPaths {
    fn library_paths(&self) -> Result<Vec<PathBuf>, OcrError> {
        if self.libraries.is_empty() {
            return Err(OcrError::LibraryNotFound {
                searched: self.libraries.clone(),
            });
        }
        Ok(self.libraries.clone())
    }

    fn tessdata_dir(&self) -> Result<PathBuf, OcrError> {
        Ok(self.tessdata_dir.clone())
    }
}

/// Development resolver: finds a Tesseract installed by the system package
/// manager (apt, Homebrew, the Windows installer).
///
/// Locations can be overridden with the `CHILLI_OCR_TESSERACT_LIB` and
/// `CHILLI_OCR_TESSDATA` environment variables — a dev/test convenience only;
/// production builds use extracted assets instead, never these env vars.
#[derive(Debug, Clone)]
pub struct SystemOcrPaths {
    library: PathBuf,
    tessdata: PathBuf,
    searched_libraries: Vec<PathBuf>,
    searched_tessdata: Vec<PathBuf>,
}

impl SystemOcrPaths {
    /// Locate a system Tesseract library and tessdata directory.
    pub fn new() -> Result<Self, OcrError> {
        let (library, searched_libraries) = locate_library();
        let library = library.ok_or_else(|| OcrError::LibraryNotFound {
            searched: searched_libraries.clone(),
        })?;
        let (tessdata, searched_tessdata) = locate_tessdata();
        let tessdata = tessdata.ok_or_else(|| OcrError::TessdataNotFound {
            searched: searched_tessdata.clone(),
        })?;
        Ok(Self {
            library,
            tessdata,
            searched_libraries,
            searched_tessdata,
        })
    }

    /// The resolved library path.
    pub fn library(&self) -> &Path {
        &self.library
    }

    /// The resolved tessdata directory.
    pub fn tessdata(&self) -> &Path {
        &self.tessdata
    }
}

impl Default for SystemOcrPaths {
    fn default() -> Self {
        Self::new().expect("a system Tesseract must be installed for SystemOcrPaths::default")
    }
}

impl OcrPaths for SystemOcrPaths {
    fn library_paths(&self) -> Result<Vec<PathBuf>, OcrError> {
        if self.library.is_file() {
            Ok(vec![self.library.clone()])
        } else {
            Err(OcrError::LibraryNotFound {
                searched: self.searched_libraries.clone(),
            })
        }
    }

    fn tessdata_dir(&self) -> Result<PathBuf, OcrError> {
        if self.tessdata.is_dir() {
            Ok(self.tessdata.clone())
        } else {
            Err(OcrError::TessdataNotFound {
                searched: self.searched_tessdata.clone(),
            })
        }
    }
}

fn locate_library() -> (Option<PathBuf>, Vec<PathBuf>) {
    let candidates = library_candidates();
    let found = candidates.iter().find(|p| p.is_file()).cloned();
    (found, candidates)
}

fn locate_tessdata() -> (Option<PathBuf>, Vec<PathBuf>) {
    let candidates = tessdata_candidates();
    let found = candidates.iter().find(|p| p.is_dir()).cloned();
    (found, candidates)
}

fn library_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = env::var_os("CHILLI_OCR_TESSERACT_LIB") {
        out.push(PathBuf::from(p));
    }

    #[cfg(target_os = "linux")]
    {
        out.push("/usr/lib/libtesseract.so".into());
        out.push("/usr/local/lib/libtesseract.so".into());
        for name in ["libtesseract.so", "libtesseract.so.5"] {
            for dir in read_subdirs("/usr/lib") {
                out.push(dir.join(name));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for name in ["libtesseract.dylib", "libtesseract.5.dylib"] {
            out.push(PathBuf::from("/opt/homebrew/lib").join(name));
            out.push(PathBuf::from("/usr/local/lib").join(name));
        }
    }
    #[cfg(windows)]
    {
        for base in [
            r"C:\Program Files\Tesseract-OCR",
            r"C:\Program Files (x86)\Tesseract-OCR",
        ] {
            for name in ["tesseract50.dll", "tesseract55.dll", "libtesseract.dll"] {
                out.push(Path::new(base).join(name));
                out.push(Path::new(base).join("x64").join(name));
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = &mut out;
    }
    out
}

fn tessdata_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = env::var_os("CHILLI_OCR_TESSDATA") {
        out.push(PathBuf::from(&p));
    }
    if let Some(p) = env::var_os("TESSDATA_PREFIX") {
        // Tesseract 4+ points TESSDATA_PREFIX at the tessdata dir directly;
        // older versions pointed at its parent. Accept both shapes.
        let path = PathBuf::from(&p);
        out.push(path.clone());
        out.push(path.join("tessdata"));
    }

    #[cfg(target_os = "linux")]
    {
        for dir in read_subdirs("/usr/share/tesseract-ocr") {
            out.push(dir.join("tessdata"));
        }
        out.push("/usr/share/tessdata".into());
        out.push("/usr/local/share/tessdata".into());
    }
    #[cfg(target_os = "macos")]
    {
        out.push("/opt/homebrew/share/tessdata".into());
        out.push("/opt/homebrew/share/tesseract-ocr/5/tessdata".into());
        out.push("/usr/local/share/tessdata".into());
    }
    #[cfg(windows)]
    {
        for base in [
            r"C:\Program Files\Tesseract-OCR\tessdata",
            r"C:\Program Files (x86)\Tesseract-OCR\tessdata",
        ] {
            out.push(base.into());
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = &mut out;
    }
    out
}

/// Immediate subdirectories of `dir`, ignoring entries that cannot be read.
///
/// Only the Linux candidate lists enumerate subdirectories; gating the helper
/// keeps it from becoming dead code (and `-D warnings` failures) on the
/// platforms with fixed candidate paths.
#[cfg(target_os = "linux")]
fn read_subdirs(dir: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(entry.path());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_paths_echo_what_was_given() {
        let paths = StaticOcrPaths::new(
            vec!["/x/libtesseract.so".into()],
            PathBuf::from("/t/tessdata"),
        );
        assert_eq!(
            paths.library_paths().unwrap(),
            vec![PathBuf::from("/x/libtesseract.so")]
        );
        assert_eq!(paths.tessdata_dir().unwrap(), PathBuf::from("/t/tessdata"));
    }

    #[test]
    fn static_paths_with_no_libraries_errors() {
        let paths = StaticOcrPaths::new(Vec::new(), PathBuf::from("/t"));
        assert!(paths.library_paths().is_err());
    }

    #[test]
    fn library_candidates_are_nonempty_on_supported_platforms() {
        // On Linux/macOS/Windows dev machines we always probe at least the
        // well-known locations; on other targets this list is empty by design.
        let candidates = library_candidates();
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        assert!(!candidates.is_empty());
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        assert!(candidates.is_empty());
    }
}
