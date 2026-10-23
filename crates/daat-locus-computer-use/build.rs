use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=native/macos.swift");
    println!("cargo:rerun-if-changed=native/virtual_display.m");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out.join("macos.o");
    let arch = if env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "aarch64" {
        "arm64"
    } else {
        "x86_64"
    };
    let target = format!("{arch}-apple-macosx11.0");
    let sdk = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .unwrap();
    assert!(sdk.status.success());
    let sdk = String::from_utf8(sdk.stdout).unwrap();
    assert!(
        Command::new("xcrun")
            .args([
                "swiftc",
                "-swift-version",
                "5",
                "-O",
                "-parse-as-library",
                "-emit-object",
                "-module-cache-path",
                out.join("swift-cache").to_str().unwrap(),
                "-target",
                &target,
                "-sdk",
                sdk.trim(),
                "native/macos.swift",
                "-o"
            ])
            .arg(&object)
            .status()
            .unwrap()
            .success(),
        "macOS native compilation failed"
    );
    let display_object = out.join("virtual_display.o");
    assert!(
        Command::new("xcrun")
            .args([
                "clang",
                "-fobjc-arc",
                "-fmodules",
                "-O2",
                "-target",
                &target,
                "-isysroot",
                sdk.trim(),
                "-c",
                "native/virtual_display.m",
                "-o"
            ])
            .arg(&display_object)
            .status()
            .unwrap()
            .success(),
        "macOS virtual display compilation failed"
    );
    assert!(
        Command::new("ar")
            .arg("crs")
            .arg(out.join("libdl_computer_use_mac.a"))
            .arg(&object)
            .arg(&display_object)
            .status()
            .unwrap()
            .success()
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    let compiler = Command::new("xcrun")
        .args(["--find", "swiftc"])
        .output()
        .unwrap();
    assert!(compiler.status.success());
    let compiler = PathBuf::from(String::from_utf8(compiler.stdout).unwrap().trim());
    println!(
        "cargo:rustc-link-search=native={}",
        compiler
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("lib/swift/macosx")
            .display()
    );
    for compatibility in ["swiftCompatibilityConcurrency", "swiftCompatibility56"] {
        println!("cargo:rustc-link-lib=static:+whole-archive={compatibility}");
    }
    println!(
        "cargo:rustc-link-search=native={}/usr/lib/swift",
        sdk.trim()
    );
    println!("cargo:rustc-link-lib=static=dl_computer_use_mac");
    println!("cargo:rustc-link-lib=swiftCore");
    for framework in [
        "AppKit",
        "ApplicationServices",
        "ScreenCaptureKit",
        "CoreGraphics",
        "Foundation",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
