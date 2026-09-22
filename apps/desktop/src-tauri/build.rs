fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon.png");
    println!("cargo:rerun-if-changed=tauri.conf.json");

    let app_manifest = tauri_build::AppManifest::new().commands(&[
        "request_shutdown",
        "minimize_to_tray",
        "exit_application",
    ]);
    let attributes = tauri_build::Attributes::new().app_manifest(app_manifest);

    tauri_build::try_build(attributes).expect("failed to build MOSS desktop application")
}
