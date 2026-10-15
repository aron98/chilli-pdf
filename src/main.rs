//! Native entry point for chilli-pdf.
//!
//! The windowed interface is built in later milestones; today the binary
//! only exposes basic CLI introspection so packaging, CI, and release
//! automation can be proven end to end on Windows, macOS, and Linux.
//! See the GUI issues on the tracker.

use chilli_pdf::VERSION;

use std::process::ExitCode;

const HELP: &str = "\
chilli-pdf — make scanned PDFs searchable, fully offline.

USAGE:
    chilli-pdf [INPUT.pdf]
    chilli-pdf --version | -V
    chilli-pdf --help | -h

Once the GUI milestone lands, running the binary (optionally with an input
PDF path) opens the native window. No accounts, no telemetry, no network.
";

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
        "No window yet: the GUI milestone is still open. \
         Run --version or --help, or follow the issue tracker."
    );
    ExitCode::FAILURE
}
