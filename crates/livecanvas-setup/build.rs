//! Embeds the application manifest and stages `livecanvas.exe` as the payload.
//!
//! Stable Cargo cannot express "build that binary first, then embed it"
//! (artifact dependencies are unstable), so the payload is looked up here:
//!   1. the path in the `LIVECANVAS_PAYLOAD` environment variable, or
//!   2. `livecanvas.exe` next to where this crate's own output will be placed.
//!
//! `build.ps1` builds the two crates in the right order. If no payload is
//! found the installer still compiles, but refuses to install at runtime and
//! says why - it never installs an empty or stale file silently.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LIVECANVAS_PAYLOAD");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    // OUT_DIR is <target>/<profile>/build/<crate>-<hash>/out; go up to <target>/<profile>.
    let profile_dir = out_dir.ancestors().nth(3).map(PathBuf::from);
    let payload = env::var_os("LIVECANVAS_PAYLOAD")
        .map(PathBuf::from)
        .or_else(|| profile_dir.map(|dir| dir.join("livecanvas.exe")));

    let staged = out_dir.join("payload.bin");
    match payload {
        Some(path) if path.is_file() => {
            println!("cargo:rerun-if-changed={}", path.display());
            fs::copy(&path, &staged).expect("copy livecanvas.exe into OUT_DIR");
        }
        other => {
            if let Some(path) = other {
                // Re-run as soon as the main binary appears.
                println!("cargo:rerun-if-changed={}", path.display());
            }
            println!(
                "cargo:warning=livecanvas.exe not found; this installer will have NO payload. \
                 Run `cargo build --release -p livecanvas` first, then build livecanvas-setup again (or use build.ps1)."
            );
            fs::write(&staged, b"").expect("write empty payload placeholder");
        }
    }

    if env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest::embed_manifest(embed_manifest::new_manifest("LiveCanvas.Setup"))
            .expect("unable to embed the application manifest");
    }
}
