fn main() {
    use std::{env, fs, path::PathBuf};
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let android = target_os == "android";
    let macos = target_os == "macos";
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
    let logo = root.join(if android || macos {
        "assets/logo-mark.svg"
    } else {
        "assets/logo.svg"
    });
    println!("cargo:rerun-if-changed={}", logo.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let tree = resvg::usvg::Tree::from_data(&fs::read(logo).unwrap(), &Default::default()).unwrap();
    let rgba = render_icon(&tree, 256, 1.0, macos);
    let png_path = out.join("app.png");
    write_png(&png_path, 256, &rgba);
    if macos {
        // 18pt at Retina resolution. A template uses alpha, so the white
        // launcher background must be removed rather than drawn as a disk.
        write_png(
            &out.join("tray.png"),
            36,
            &render_icon(&tree, 36, 1.3, true),
        );
    }
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
                "browser_control",
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

fn render_icon(tree: &resvg::usvg::Tree, size: u32, zoom: f32, transparent_mark: bool) -> Vec<u8> {
    let mut pixels = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
    let scale = size as f32 / tree.size().width() * zoom;
    let offset = (size as f32 - tree.size().width() * scale) / 2.0;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(offset, offset),
        &mut pixels.as_mut(),
    );
    let mut rgba = pixels.data().to_vec();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        if transparent_mark {
            // The source is black on white; premultiplied alpha minus red
            // retains only the black mark, including its antialiased edges.
            pixel[3] = pixel[3].saturating_sub(pixel[0]);
            pixel[..3].fill(0);
            continue;
        }
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for color in &mut pixel[..3] {
                *color = ((*color as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    rgba
}

fn write_png(path: &std::path::Path, size: u32, rgba: &[u8]) {
    let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(rgba)
        .unwrap();
}
