fn main() {
    println!("cargo:rerun-if-env-changed=SYNTHCHAT_UPDATE_MANIFEST_URL");

    #[cfg(target_os = "windows")]
    build_windows();

    #[cfg(not(target_os = "windows"))]
    tauri_build::build()
}

#[cfg(target_os = "windows")]
fn build_windows() {
    println!("cargo:rerun-if-changed=windows-test-manifest.rc");
    println!("cargo:rerun-if-changed=windows-test-manifest.xml");
    // The desktop startup path can deserialize and normalize a large state file before
    // control reaches Tokio worker threads. Windows' default main-thread stack is too
    // small for that path in release builds, causing STATUS_STACK_OVERFLOW on launch.
    println!("cargo:rustc-link-arg-bin=synthchat-v1=/STACK:67108864");
    embed_resource::compile_for_everything("windows-test-manifest.rc", embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed Windows app manifest");

    let windows = tauri_build::WindowsAttributes::new_without_app_manifest();
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attributes).expect("failed to run Tauri build script");
}
