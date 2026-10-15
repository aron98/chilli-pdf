//! Output-path rules: default naming, collision handling, and the
//! never-overwrite-the-input guarantee.

use std::path::{Path, PathBuf};

/// Suffix inserted before the extension of the original file name.
pub const SUFFIX: &str = "_searchable";

/// Problems detected before any conversion output is written.
#[derive(Debug, PartialEq, Eq)]
pub enum Conflict {
    /// The chosen output resolves to the input file; writing it would
    /// destroy the original. Always a hard error — never ask the user
    /// to confirm this.
    SameAsInput,
    /// A file already exists at the output location. The GUI must ask
    /// the user before replacing it.
    AlreadyExists(PathBuf),
}

/// Default output location: same directory, `<original stem>_searchable.pdf`.
///
/// `scan.pdf` becomes `scan_searchable.pdf`. The extension is replaced, the
/// directory is preserved untouched (including spaces and Unicode), and only
/// the final `.pdf` suffix of a multi-dot name is treated as the extension.
pub fn default_output_path(input: &Path) -> PathBuf {
    let directory = input.parent().unwrap_or_else(|| Path::new(""));
    let stem = input
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".to_owned());
    directory.join(format!("{stem}{SUFFIX}.pdf"))
}

/// Pre-flight check before conversion writes anything.
///
/// The GUI calls this when the user confirms the output location.
/// [`Conflict::AlreadyExists`] is the "ask before replacing" case;
/// [`Conflict::SameAsInput`] is always fatal.
pub fn check_output(input: &Path, output: &Path) -> Result<(), Conflict> {
    if same_location(input, output) {
        return Err(Conflict::SameAsInput);
    }
    if output.exists() {
        return Err(Conflict::AlreadyExists(output.to_path_buf()));
    }
    Ok(())
}

/// Decide whether two paths refer to the same file.
///
/// Canonicalized comparison when both files exist; a conservative
/// case-insensitive fallback otherwise (so Windows users cannot select
/// `SCAN.PDF` itself as the output of `scan.pdf`).
fn same_location(input: &Path, output: &Path) -> bool {
    match (input.canonicalize(), output.canonicalize()) {
        (Ok(input), Ok(output)) => input == output,
        _ => {
            let fold = |path: &Path| path.to_string_lossy().to_lowercase();
            fold(input) == fold(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chilli-pdf-tests-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_name_appends_suffix_and_forces_pdf_extension() {
        assert_eq!(
            default_output_path(Path::new("/scans/report.pdf")),
            PathBuf::from("/scans/report_searchable.pdf")
        );
    }

    #[test]
    fn default_name_keeps_multi_dot_stems() {
        assert_eq!(
            default_output_path(Path::new("/scans/2024 report.final.pdf")),
            PathBuf::from("/scans/2024 report.final_searchable.pdf")
        );
    }

    #[test]
    fn default_name_handles_missing_extension() {
        assert_eq!(
            default_output_path(Path::new("/scans/plain")),
            PathBuf::from("/scans/plain_searchable.pdf")
        );
    }

    #[test]
    fn unicode_names_and_directories_are_preserved() {
        assert_eq!(
            default_output_path(Path::new("/mappák/Magyar szöveg.pdf")),
            PathBuf::from("/mappák/Magyar szöveg_searchable.pdf")
        );
    }

    #[test]
    fn stemless_paths_fall_back_to_document() {
        assert_eq!(
            default_output_path(Path::new("/")),
            PathBuf::from("document_searchable.pdf")
        );
    }

    #[test]
    fn fresh_output_location_is_accepted() {
        let dir = scratch_dir();
        let input = dir.join("in.pdf");
        let output = dir.join("in_searchable.pdf");
        fs::write(&input, b"x").unwrap();
        assert_eq!(check_output(&input, &output), Ok(()));
    }

    #[test]
    fn writing_the_input_itself_is_refused() {
        let dir = scratch_dir();
        let input = dir.join("in.pdf");
        fs::write(&input, b"x").unwrap();
        assert_eq!(check_output(&input, &input), Err(Conflict::SameAsInput));
    }

    #[test]
    fn existing_output_is_reported_as_a_collision() {
        let dir = scratch_dir();
        let input = dir.join("in.pdf");
        let output = dir.join("out_searchable.pdf");
        fs::write(&input, b"x").unwrap();
        fs::write(&output, b"existing").unwrap();
        assert_eq!(
            check_output(&input, &output),
            Err(Conflict::AlreadyExists(output.clone()))
        );
    }
}
