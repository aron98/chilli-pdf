//! Native entry point for chilli-pdf.
//!
//! With the `gui` feature (default), running the binary opens the native
//! converter window — optionally with an input PDF path pre-selected from
//! the command line. `--version`/`-V` and `--help`/`-h` keep working for
//! packaging and scripting.
//!
//! Without the `gui` feature the binary is a headless stub: display-less
//! builds and CI compile and test the core logic without any GUI toolkit.

use chilli_pdf::VERSION;

use std::process::ExitCode;

const HELP: &str = "\
chilli-pdf — make scanned PDFs searchable, fully offline.

USAGE:
    chilli-pdf [INPUT.pdf]
    chilli-pdf --version | -V
    chilli-pdf --help | -h

Running the binary opens the native window, optionally with INPUT.pdf
pre-selected. No accounts, no telemetry, no network.
";

#[cfg(feature = "gui")]
fn main() -> ExitCode {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    for arg in &args {
        if arg == "--version" || arg == "-V" {
            println!("chilli-pdf {VERSION}");
            return ExitCode::SUCCESS;
        }
        if arg == "--help" || arg == "-h" {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
    }

    // Pre-select the input when the binary is started with a PDF path.
    let initial_input = args
        .iter()
        .find(|arg| !arg.to_string_lossy().starts_with('-'))
        .cloned()
        .map(std::path::PathBuf::from);

    match chilli_pdf::app::run(initial_input) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Could not open the window: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "gui"))]
fn main() -> ExitCode {
    for arg in std::env::args_os().skip(1) {
        if arg == "--version" || arg == "-V" {
            println!("chilli-pdf {VERSION}");
            return ExitCode::SUCCESS;
        }
        if arg == "--help" || arg == "-h" {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
    }

    eprintln!(
        "This build has no GUI (compiled without the `gui` feature). \
         Run --version or --help."
    );
    ExitCode::FAILURE
}
