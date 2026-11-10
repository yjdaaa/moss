use std::{env, fs, path::PathBuf};

fn main() {
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let dist = manifest.join("../../frontend/dist");

    // rust-embed snapshots dist/ at compile time in release mode. Without this
    // dependency Cargo can reuse a stale laser-api even after Vite is rebuilt.
    println!("cargo:rerun-if-env-changed=MOSS_RELEASE_VERSION");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!(
        "cargo:rerun-if-changed={}",
        dist.join("index.html").display()
    );

    if env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }

    // A successful Rust/NSIS build with an empty embedded frontend causes a
    // white page with {"result":"未找到路径"} at the desktop / route.
    // Fail the release BUILD rather than producing another unusable installer.
    let index = fs::read_to_string(dist.join("index.html")).unwrap_or_else(|error| {
        panic!("MOSS release frontend missing; run npm run build in frontend/: {error}");
    });
    assert!(
        index.contains("<div id=\"root\"></div>") && index.contains("/assets/"),
        "MOSS release frontend index.html lacks the React root or Vite assets"
    );
    let assets = fs::read_dir(dist.join("assets")).unwrap_or_else(|error| {
        panic!("MOSS release frontend assets missing; run npm run build: {error}");
    });
    assert!(
        assets.filter_map(Result::ok).any(|entry| {
            entry.path().extension().is_some_and(|ext| ext == "js")
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        }),
        "MOSS release frontend assets must contain a non-empty JavaScript bundle"
    );
}
