//! Transactional output: temp sibling → fsync → atomic rename.
//!
//! The final file must only ever appear complete: conversion writes to a
//! temporary file **in the same directory as the destination**, fsyncs it,
//! and only then renames it into place. Any failure — write error,
//! cancellation, full disk, closed file handle — deletes the temp file,
//! so cancel/crash/failure leaves the original input intact, the previous
//! output (if any) intact, and no misleading partial output.
//!
//! # Composition with `output_path`
//!
//! The GUI flow is: [`output_path::default_output_path`] → user picks a
//! location → [`output_path::check_output`] reports a [`Conflict`] → the
//! user answers the replace prompt → [`publish_document`] performs the
//! conflict check and the transactional write in one call. `Conflict`
//! detection is authoritative for the ask-before-replace flow; this module
//! additionally guards the commit instant (a file may appear between the
//! prompt and the rename).
//!
//! # Placement and rename guarantees
//!
//! The temp file is created *next to the destination* (never in `/tmp`),
//! so the final rename is same-directory and cannot fail cross-device,
//! cannot be affected by a full system-temp disk, and inherits the
//! destination's filesystem permissions. On Windows `rename` refuses to
//! clobber, so a confirmed replace falls back to remove-then-rename for
//! that one case; on POSIX the rename atomically replaces.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::output_path::{self, Conflict};

/// Fragment that identifies every temp file this module creates.
///
/// Startup cleanup (see [`cleanup_temp_files`]) only touches names
/// containing this marker, so it can never delete unrelated user files.
pub const TEMP_MARKER: &str = ".chilli-pdf-tmp";

/// Monotonic suffix so concurrent conversions in one directory never
/// collide on the same temp name.
static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Policy for a destination file that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfExists {
    /// Refuse and report [`TransactionError::DestinationExists`] — the
    /// GUI only allows this after the user declined the replace prompt.
    Refuse,
    /// Replace the existing file. The GUI only allows this after the user
    /// explicitly confirmed replacement.
    Replace,
}

/// Produces the converted document into the provided sink.
///
/// Implementations stream page by page (bounded memory) and signal
/// cancellation or failure by returning `Err`, which the transactional
/// writer turns into cleanup + [`TransactionError::WriteFailed`]; the
/// destination is never touched in that case.
pub trait DocumentWriter {
    /// Write the complete output document to `sink`.
    fn write_document(&mut self, sink: &mut dyn Write) -> io::Result<()>;
}

impl DocumentWriter for &[u8] {
    fn write_document(&mut self, sink: &mut dyn Write) -> io::Result<()> {
        sink.write_all(self)
    }
}

/// Adapter that lets a closure act as a [`DocumentWriter`], for tests and
/// future pipeline code that already owns the bytes.
pub struct FnWriter<F>(pub F);

impl<F> DocumentWriter for FnWriter<F>
where
    F: FnMut(&mut dyn Write) -> io::Result<()>,
{
    fn write_document(&mut self, sink: &mut dyn Write) -> io::Result<()> {
        (self.0)(sink)
    }
}

/// Failure of the transactional write, all mapped to user-facing,
/// content-free messages. The original input and any pre-existing output
/// are always left untouched when this is returned.
#[derive(Debug)]
pub enum TransactionError {
    /// The destination file already exists and the policy was
    /// [`IfExists::Refuse`]. Ask the user before replacing.
    DestinationExists(PathBuf),
    /// The temp file could not be created in the destination directory
    /// (missing/unwritable directory, permissions, full disk).
    UnwritableDestination(io::Error),
    /// The document write, or its fsync, failed partway (including
    /// cancellation signalled by the writer). Temp removed, destination
    /// untouched.
    WriteFailed(io::Error),
    /// The finished, fsynced temp file could not be renamed into place.
    /// Temp removed, destination untouched.
    PublishFailed(io::Error),
}

impl TransactionError {
    /// User-facing message; never contains document contents. The
    /// underlying `io::Error` (path, OS message) stays on the error value
    /// for logs/diagnostics instead.
    pub fn user_message(&self) -> String {
        match self {
            Self::DestinationExists(_) => concat!(
                "A file already exists at the chosen output location. ",
                "Pick a different name or folder, or confirm replacing it."
            )
            .to_owned(),
            Self::UnwritableDestination(_) => concat!(
                "The output folder could not be written to. Check that the ",
                "folder exists and you have permission to save there, then ",
                "try again."
            )
            .to_owned(),
            Self::WriteFailed(_) => concat!(
                "Writing the document failed partway through, so nothing ",
                "was saved and your original file is untouched. Free up disk ",
                "space and try again."
            )
            .to_owned(),
            Self::PublishFailed(_) => concat!(
                "The finished document could not be put in place during the ",
                "final step of saving. Nothing was replaced; try saving to ",
                "another folder."
            )
            .to_owned(),
        }
    }
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationExists(path) => {
                write!(f, "destination already exists: {}", path.display())
            }
            Self::UnwritableDestination(err) => {
                write!(f, "cannot create temp file in destination directory: {err}")
            }
            Self::WriteFailed(err) => write!(f, "document write failed: {err}"),
            Self::PublishFailed(err) => {
                write!(f, "atomic rename into place failed: {err}")
            }
        }
    }
}

impl std::error::Error for TransactionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DestinationExists(_) => None,
            Self::UnwritableDestination(err)
            | Self::WriteFailed(err)
            | Self::PublishFailed(err) => Some(err),
        }
    }
}

/// Full pre-flight + write flow for the GUI: refuse same-as-input, ask
/// before replacing existing output, then write transactionally.
///
/// [`Conflict::SameAsInput`] is always fatal (the original can never be
/// the destination). [`Conflict::AlreadyExists`] surfaces as
/// [`PublishError::Conflict`] when the policy is [`IfExists::Refuse`],
/// which is the signal for the GUI to show the replace prompt; on
/// confirmation it calls again with [`IfExists::Replace`].
pub fn publish_document(
    input: &Path,
    output: &Path,
    writer: &mut dyn DocumentWriter,
    if_exists: IfExists,
) -> Result<(), PublishError> {
    if let Err(conflict) = output_path::check_output(input, output) {
        let refuse_existing = if_exists == IfExists::Refuse;
        if matches!(conflict, Conflict::SameAsInput)
            || (refuse_existing && matches!(conflict, Conflict::AlreadyExists(_)))
        {
            return Err(PublishError::Conflict(conflict));
        }
    }
    write_document_transactionally(output, writer, if_exists).map_err(PublishError::Transaction)
}

/// Failure of [`publish_document`], combining the pre-flight conflicts
/// from [`output_path::check_output`] with [`TransactionError`].
#[derive(Debug)]
pub enum PublishError {
    /// Pre-flight refused the write (same-as-input, or existing output
    /// without confirmed replacement).
    Conflict(Conflict),
    /// The transactional write itself failed.
    Transaction(TransactionError),
}

impl PublishError {
    /// User-facing message; never contains document contents.
    pub fn user_message(&self) -> String {
        match self {
            Self::Conflict(Conflict::SameAsInput) => concat!(
                "The chosen output location is the original input file. ",
                "chilli-pdf never overwrites the original — pick a ",
                "different name or folder."
            )
            .to_owned(),
            Self::Conflict(Conflict::AlreadyExists(_)) => concat!(
                "A file already exists at the chosen output location. ",
                "Pick a different name or folder, or confirm replacing it."
            )
            .to_owned(),
            Self::Transaction(err) => err.user_message(),
        }
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(Conflict::SameAsInput) => {
                f.write_str("output location is the input file")
            }
            Self::Conflict(Conflict::AlreadyExists(path)) => {
                write!(f, "output already exists: {}", path.display())
            }
            Self::Transaction(err) => write!(f, "transactional write failed: {err}"),
        }
    }
}

impl std::error::Error for PublishError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conflict(_) => None,
            Self::Transaction(err) => Some(err),
        }
    }
}

impl From<TransactionError> for PublishError {
    fn from(err: TransactionError) -> Self {
        Self::Transaction(err)
    }
}

/// Write `writer`'s document to `destination` transactionally: temp
/// sibling in the same directory → fsync → atomic rename.
///
/// On any failure (including a cancellation signalled by `writer`) the
/// temp file is deleted and the destination is left exactly as it was.
pub fn write_document_transactionally(
    destination: &Path,
    writer: &mut dyn DocumentWriter,
    if_exists: IfExists,
) -> Result<(), TransactionError> {
    // Empty parent (bare file name) means "current directory".
    let directory = match destination.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };

    if destination.exists() && if_exists == IfExists::Refuse {
        return Err(TransactionError::DestinationExists(
            destination.to_path_buf(),
        ));
    }

    let temp_path = temp_path_for(destination, &directory);
    let result = commit_via_temp(&temp_path, destination, writer, if_exists);
    if result.is_err() {
        // Failure, cancellation, or refused replacement: no leftovers.
        remove_best_effort(&temp_path);
    }
    result
}

/// Create the temp sibling, stream the document into it, fsync, and
/// atomically rename it onto `destination`.
fn commit_via_temp(
    temp_path: &Path,
    destination: &Path,
    writer: &mut dyn DocumentWriter,
    if_exists: IfExists,
) -> Result<(), TransactionError> {
    let mut file = File::create(temp_path).map_err(TransactionError::UnwritableDestination)?;

    let write_result = writer.write_document(&mut file);
    // fsync regardless of write outcome so a failing disk surfaces here
    // too; both cases clean up and leave the destination untouched.
    let sync_result = write_result.and_then(|()| file.sync_all());

    // Close the handle before renaming (required on Windows).
    drop(file);

    sync_result.map_err(TransactionError::WriteFailed)?;

    match fs::rename(temp_path, destination) {
        Ok(()) => {
            // Best-effort durability of the rename itself; ignoring the
            // result is safe because the content is already durable.
            sync_directory_best_effort(temp_path.parent());
            Ok(())
        }
        Err(err) => {
            // Windows refuses to clobber an existing destination. With a
            // confirmed replacement, remove it and retry; anything else is
            // a genuine publish failure.
            if destination.exists() && if_exists == IfExists::Replace {
                if let Err(remove_err) = fs::remove_file(destination) {
                    return Err(TransactionError::PublishFailed(remove_err));
                }
                return fs::rename(temp_path, destination).map_err(TransactionError::PublishFailed);
            }
            Err(TransactionError::PublishFailed(err))
        }
    }
}

/// Unique temp sibling of `destination` inside `directory`.
///
/// The name embeds [`TEMP_MARKER`] plus the process id and a monotonic
/// counter, is dot-prefixed (hidden on Unix), and can only exist while a
/// conversion is running or after a crash — which is what
/// [`cleanup_temp_files`] sweeps.
fn temp_path_for(destination: &Path, directory: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document.pdf".to_owned());
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    directory.join(format!(".{name}{TEMP_MARKER}-{pid}-{unique}.tmp"))
}

/// True when `name` follows this module's temp-file naming pattern.
pub fn is_temp_file_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.contains(TEMP_MARKER) && name.ends_with(".tmp"))
}

/// Best-effort removal; temp cleanup must never mask the real error.
fn remove_best_effort(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}

/// Best-effort directory fsync after the rename (POSIX semantics; on
/// platforms where directories cannot be opened this is a no-op).
fn sync_directory_best_effort(directory: Option<&Path>) {
    if let Some(directory) = directory {
        if let Ok(dir_file) = File::open(directory) {
            let _ = dir_file.sync_all();
        }
    }
}

/// Delete leftover temp files from crashed conversions.
///
/// Call once at startup for the (single) output directory in use. Only
/// names matching [`is_temp_file_name`] are touched, so unrelated user
/// files are never removed. Returns the number of files deleted.
/// Best-effort: unreadable entries are skipped, and a fresh temp file
/// from a *concurrent* conversion whose name does not belong to this
/// process is still removed if it is older than the call.
pub fn cleanup_temp_files(directory: &Path) -> usize {
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if is_temp_file_name(&entry.file_name()) && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chilli-pdf-transactional-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn leftover_temps(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.file_name().is_some_and(|name| is_temp_file_name(name)))
            .collect()
    }

    fn sink_file(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap()
    }

    /// Bytes with an explicit `&[u8]` type so `&mut content` coerces to
    /// `&mut dyn DocumentWriter` via the `impl for &[u8]`.
    const CONTENT: &[u8] = b"%PDF-1.4 converted document bytes";

    #[test]
    fn successful_write_publishes_exactly_the_document_bytes() {
        let dir = scratch_dir("success");
        let dest = dir.join("out_searchable.pdf");
        let mut content = CONTENT;

        write_document_transactionally(&dest, &mut content, IfExists::Refuse).unwrap();

        assert_eq!(sink_file(&dest), CONTENT);
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn failed_mid_write_leaves_no_final_file_and_no_temp() {
        let dir = scratch_dir("fail-mid");
        let dest = dir.join("out_searchable.pdf");

        let mut writer = FnWriter(|sink: &mut dyn Write| {
            sink.write_all(b"partial content")?;
            Err(io::Error::other("simulated disk failure mid-write"))
        });
        let err = write_document_transactionally(&dest, &mut writer, IfExists::Refuse).unwrap_err();

        assert!(matches!(err, TransactionError::WriteFailed(_)));
        assert!(!dest.exists(), "no partial file at the final path");
        assert!(leftover_temps(&dir).is_empty(), "no temp leftovers");
        assert!(err.user_message().contains("nothing"));
    }

    #[test]
    fn cancelled_write_leaves_no_final_file_and_no_temp() {
        let dir = scratch_dir("cancel");
        let dest = dir.join("out_searchable.pdf");

        let mut writer = FnWriter(|sink: &mut dyn Write| {
            sink.write_all(b"half a page")?;
            Err(io::Error::other("cancel requested between pages"))
        });
        let err = write_document_transactionally(&dest, &mut writer, IfExists::Refuse).unwrap_err();

        assert!(matches!(err, TransactionError::WriteFailed(_)));
        assert!(!dest.exists(), "cancelled conversion must publish nothing");
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn failed_write_preserves_an_existing_confirmed_destination() {
        let dir = scratch_dir("fail-replace");
        let dest = dir.join("out_searchable.pdf");
        fs::write(&dest, b"previous document").unwrap();

        let mut writer =
            FnWriter(|_sink: &mut dyn Write| Err(io::Error::other("disk full after a few pages")));
        let err =
            write_document_transactionally(&dest, &mut writer, IfExists::Replace).unwrap_err();

        assert!(matches!(err, TransactionError::WriteFailed(_)));
        assert_eq!(
            sink_file(&dest),
            b"previous document",
            "existing output must survive a failed replace"
        );
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn existing_destination_is_refused_without_confirmation() {
        let dir = scratch_dir("refuse");
        let dest = dir.join("out_searchable.pdf");
        let mut content = CONTENT;
        fs::write(&dest, b"previous document").unwrap();

        let err =
            write_document_transactionally(&dest, &mut content, IfExists::Refuse).unwrap_err();

        assert!(matches!(err, TransactionError::DestinationExists(_)));
        assert_eq!(sink_file(&dest), b"previous document");
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn existing_destination_is_replaced_after_confirmation() {
        let dir = scratch_dir("replace");
        let dest = dir.join("out_searchable.pdf");
        let mut content = CONTENT;
        fs::write(&dest, b"previous document").unwrap();

        write_document_transactionally(&dest, &mut content, IfExists::Replace).unwrap();

        assert_eq!(sink_file(&dest), CONTENT);
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn unwritable_destination_reports_a_clear_error() {
        let dir = scratch_dir("unwritable");
        let mut content = CONTENT;
        let dest = dir.join("no such folder").join("out_searchable.pdf");

        let err =
            write_document_transactionally(&dest, &mut content, IfExists::Refuse).unwrap_err();

        assert!(matches!(err, TransactionError::UnwritableDestination(_)));
        assert!(!dest.exists());
        assert!(err.user_message().contains("folder"));
    }

    #[test]
    fn unicode_and_dotfile_destinations_are_handled() {
        let dir = scratch_dir("unicode");
        let mut content = CONTENT;
        for name in ["Magyar szöveg_searchable.pdf", ".hidden_searchable.pdf"] {
            let dest = dir.join(name);
            write_document_transactionally(&dest, &mut content, IfExists::Refuse).unwrap();
            assert_eq!(sink_file(&dest), CONTENT, "destination {name}");
        }
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn temp_files_live_next_to_the_destination_not_in_system_temp() {
        let dir = scratch_dir("same-dir");
        let dest = dir.join("out_searchable.pdf");

        let mut writer = FnWriter(|_sink: &mut dyn Write| {
            // Temp file must already exist in `dir` while the writer runs.
            assert_eq!(leftover_temps(&dir).len(), 1);
            Err(io::Error::other("inspect the temp path"))
        });
        let _ = write_document_transactionally(&dest, &mut writer, IfExists::Refuse);

        assert!(leftover_temps(&dir).is_empty(), "cleaned up after failure");
    }

    #[test]
    fn publish_refuses_same_as_input_regardless_of_policy() {
        let dir = scratch_dir("same-input");
        let input = dir.join("in.pdf");
        fs::write(&input, CONTENT).unwrap();
        let mut content = CONTENT;

        for policy in [IfExists::Refuse, IfExists::Replace] {
            let err = publish_document(&input, &input, &mut content, policy).unwrap_err();
            assert!(
                matches!(err, PublishError::Conflict(Conflict::SameAsInput)),
                "input can never be the destination"
            );
            assert_eq!(sink_file(&input), CONTENT);
        }
    }

    #[test]
    fn publish_asks_before_replacing_an_existing_output() {
        let dir = scratch_dir("ask");
        let input = dir.join("in.pdf");
        let output = dir.join("in_searchable.pdf");
        fs::write(&input, b"input document").unwrap();
        fs::write(&output, b"old output").unwrap();
        let mut content = CONTENT;

        // Refuse (user has not confirmed): conflict surfaces, nothing
        // is written.
        let err = publish_document(&input, &output, &mut content, IfExists::Refuse).unwrap_err();
        assert!(matches!(
            err,
            PublishError::Conflict(Conflict::AlreadyExists(_))
        ));
        assert_eq!(sink_file(&output), b"old output");

        // Replace (user confirmed): new content lands atomically.
        publish_document(&input, &output, &mut content, IfExists::Replace).unwrap();
        assert_eq!(sink_file(&output), CONTENT);
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn publish_writes_fresh_outputs_without_conflicts() {
        let dir = scratch_dir("fresh");
        let mut content = CONTENT;
        let input = dir.join("in.pdf");
        let output = dir.join("in_searchable.pdf");
        fs::write(&input, b"input document").unwrap();

        publish_document(&input, &output, &mut content, IfExists::Refuse).unwrap();

        assert_eq!(sink_file(&output), CONTENT);
        assert!(leftover_temps(&dir).is_empty());
    }

    #[test]
    fn cleanup_removes_only_chilli_temp_files() {
        let dir = scratch_dir("cleanup");
        fs::write(dir.join("keep.pdf"), b"precious").unwrap();
        fs::write(dir.join("ordinary.tmp"), b"not ours").unwrap();
        let stale_name = format!(".out_searchable.pdf{TEMP_MARKER}-99999-0.tmp");
        let stale = dir.join(stale_name);
        fs::write(&stale, b"crashed conversion leftovers").unwrap();

        let removed = cleanup_temp_files(&dir);

        assert_eq!(removed, 1);
        assert!(dir.join("keep.pdf").exists(), "user files are untouched");
        assert!(
            dir.join("ordinary.tmp").exists(),
            "foreign .tmp files are untouched"
        );
        assert!(!stale.exists(), "our stale temp file is removed");
    }

    #[test]
    fn cleanup_on_a_missing_directory_is_harmless() {
        assert_eq!(cleanup_temp_files(Path::new("/definitely/not/here")), 0);
    }
}
