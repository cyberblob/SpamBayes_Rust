//! Build script for spambayes-mapi
//!
//! Emits the correct Windows SDK library search path for the target architecture
//! so that `mapi32.lib` (and other UM libs) resolve to the matching bitness.

fn main() {
    // Determine the architecture subfolder based on the cargo target
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let lib_arch = match arch.as_str() {
        "x86" => "x86",
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => "x64",
    };

    // Try to locate the Windows SDK UM lib path.
    // We check known SDK versions in descending order.
    let sdk_root = r"C:\Program Files (x86)\Windows Kits\10\Lib";

    let sdk_versions = [
        "10.0.26100.0",
        "10.0.22621.0",
        "10.0.22000.0",
        "10.0.20348.0",
        "10.0.19041.0",
        "10.0.18362.0",
    ];

    for version in &sdk_versions {
        let um_path = format!("{sdk_root}\\{version}\\um\\{lib_arch}");
        if std::path::Path::new(&um_path).exists() {
            println!("cargo:rustc-link-search=native={um_path}");
            break;
        }
    }

    // Re-run only if this script changes
    println!("cargo:rerun-if-changed=build.rs");
}
