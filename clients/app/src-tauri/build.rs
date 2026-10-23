fn main() {
    use std::{env, fs, path::PathBuf};
    let android = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android");
    if android {
        // NDK 27 needs this opt-in for Android devices with 16 KiB pages.
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
        // APS2 relocations are supported since API 23 (our minimum is 24).
        println!("cargo:rustc-link-arg=-Wl,--pack-dyn-relocs=android");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../..");
    if env::var_os("CARGO_FEATURE_CUSTOM_PROTOCOL").is_some() {
        let dist = root.join("webui/dist");
        println!("cargo:rerun-if-changed={}", dist.display());
        assert!(
            dist.join("index.html").is_file(),
            "Build the shared WebUI before the native client: bun scripts/build-webui.ts"
        );
    }
    let logo = root.join(if android {
        "assets/logo-mark.svg"
    } else {
        "assets/logo.svg"
    });
    println!("cargo:rerun-if-changed={}", logo.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let tree = resvg::usvg::Tree::from_data(&fs::read(logo).unwrap(), &Default::default()).unwrap();
    let mut pixels = resvg::tiny_skia::Pixmap::new(256, 256).unwrap();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(
            256.0 / tree.size().width(),
            256.0 / tree.size().height(),
        ),
        &mut pixels.as_mut(),
    );
    let mut rgba = pixels.data().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for color in &mut pixel[..3] {
                *color = ((*color as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    let png_path = out.join("app.png");
    let mut encoder = png::Encoder::new(fs::File::create(&png_path).unwrap(), 256, 256);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgba)
        .unwrap();
    let icon_path = out.join("app.ico");
    let mut icons = ico::IconDir::new(ico::ResourceType::Icon);
    icons.add_entry(
        ico::IconDirEntry::encode(&ico::IconImage::from_rgba_data(256, 256, rgba)).unwrap(),
    );
    icons.write(fs::File::create(&icon_path).unwrap()).unwrap();
    let mut config: serde_json::Value = env::var("TAURI_CONFIG")
        .ok()
        .map(|value| serde_json::from_str(&value).unwrap())
        .unwrap_or_else(|| serde_json::json!({}));
    if config.get("bundle").is_none() {
        config["bundle"] = serde_json::json!({});
    }
    config["bundle"]["icon"] = serde_json::json!([png_path]);
    let config = config.to_string();
    // The build helper and context macro consume the same generated icon configuration.
    unsafe {
        env::set_var("TAURI_CONFIG", &config);
    }
    println!("cargo:rustc-env=TAURI_CONFIG={config}");
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&[
                "desktop_account",
                "desktop_window",
                "phone_use_status",
                "phone_use_configure",
                "phone_use_display_configure",
                "computer_use_status",
                "computer_use_configure",
                "computer_use_display_configure",
                "computer_use_permission",
                "native_http_start",
                "native_http_write",
                "native_http_send",
                "native_http_read",
                "native_http_cancel",
                "native_resource_download",
            ]))
            .windows_attributes(tauri_build::WindowsAttributes::new().window_icon_path(icon_path)),
    )
    .unwrap();
}
