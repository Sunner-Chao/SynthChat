fn main() {
    #[cfg(target_os = "windows")]
    build_windows();

    #[cfg(not(target_os = "windows"))]
    tauri_build::build()
}

#[cfg(target_os = "windows")]
fn build_windows() {
    println!("cargo:rerun-if-changed=windows-test-manifest.rc");
    println!("cargo:rerun-if-changed=windows-test-manifest.xml");
    embed_resource::compile_for_everything("windows-test-manifest.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed Windows app manifest");

    let windows = tauri_build::WindowsAttributes::new_without_app_manifest();
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attributes).expect("failed to run Tauri build script");
}
