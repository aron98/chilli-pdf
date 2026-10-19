//! Build-time asset-bundle selection (issue #19).
//!
//! chilli-pdf embeds its asset bundle into the binary at compile time:
//!
//! * **fixture mode (default)** — the committed `assets/v1/` fixture
//!   bundle. Development needs no network: the tiny placeholder blobs
//!   keep every build and test working offline.
//! * **real-assets mode** — when `scripts/fetch-assets` has downloaded
//!   the pinned real libraries and models into the gitignored
//!   `assets/real/` overlay, the crate embeds **those** instead.
//!
//! This script emits `--cfg chilli_real_assets` in real-assets mode so
//! `src/assets.rs` can switch its `include_dir!`/`include_str!` targets.
//!
//! `cargo:rerun-if-changed=assets` makes cargo watch the whole `assets/`
//! tree — **including** the gitignored `assets/real/` overlay. This is
//! required: the default "rerun when any package file changes" behavior
//! respects `.gitignore`, so creating or removing the overlay would
//! otherwise never re-run this script and the mode would silently stay
//! as it was on the previous build. Explicit rerun-if paths bypass the
//! ignore filtering, so `scripts/fetch-assets` (and `rm -rf assets/real`)
//! flip the mode on the very next build.
//!
//! The script is std-only and never touches the network; the fetching
//! itself lives in `scripts/fetch-assets` (see `docs/packaging.md`).

use std::path::Path;

fn main() {
    // Declaring the cfg silences the `unexpected_cfgs` lint in both modes.
    println!("cargo:rustc-check-cfg=cfg(chilli_real_assets)");

    // Watch the whole assets tree so that creating or removing the
    // gitignored assets/real/ overlay re-runs this script. See the module
    // docs above for why the default package-change tracking is not enough.
    println!("cargo:rerun-if-changed=assets");

    let real_bundle = Path::new("assets/real/v1");
    let real_manifest = Path::new("assets/real/manifest.txt");
    if real_bundle.is_dir() && real_manifest.is_file() {
        println!("cargo:rustc-cfg=chilli_real_assets");
        println!(
            "cargo:warning=chilli-pdf: embedding REAL bundled assets from assets/real/v1 \
             (run `rm -rf assets/real` to return to the fixture bundle)"
        );
    }
}
