use std::{env, path::PathBuf};

fn main() {
    assert_eq!(
        env::var("TARGET").unwrap(),
        "x86_64-pc-windows-msvc",
        "build the IDD with cargo run -p xtask -- idd-driver for Windows x64"
    );
    for name in ["DL_IDD_INCLUDE", "LIB"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed=wrapper.h");
    let includes = env::var("DL_IDD_INCLUDE")
        .expect("use cargo run -p xtask -- idd-driver to configure the WDK");
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_args(["-x", "c++", "--target=x86_64-pc-windows-msvc", "-fms-extensions",
            "-DUMDF_DRIVER", "-DUMDF_USING_NTSTATUS", "-DUMDF_VERSION_MAJOR=2",
            "-DUMDF_VERSION_MINOR=25", "-DIDDCX_VERSION_MAJOR=1", "-DIDDCX_VERSION_MINOR=4",
            "-DUNICODE", "-D_UNICODE", "-D_AMD64_", "-D_WIN64", "-D_WIN32_WINNT=0x0A00"])
        .allowlist_type("(WDF|_WDF|PWDF|PFN_WDF|IDDCX|IDDFUNCENUM|IDD_|IDARG|PFN_IDD|DISPLAYCONFIG|LUID|GUID|NTSTATUS|UNICODE_STRING|DRIVER_OBJECT).*" )
        .allowlist_var("(Wdf.*|Idd.*|.*TableIndex)")
        .blocklist_item("IddMinimumVersionRequired")
        .derive_default(true)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    for directory in includes.split(';') {
        builder = builder.clang_arg(format!("-I{directory}"));
    }
    builder
        .generate()
        .expect("WDK binding generation")
        .write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("bindings.rs"))
        .expect("write WDK bindings");
    for directory in env::var("LIB").unwrap().split(';') {
        println!("cargo:rustc-link-search=native={directory}");
    }
    println!("cargo:rustc-link-lib=static=IddCxStub");
    println!("cargo:rustc-link-lib=ntdll");
    println!("cargo:rustc-cdylib-link-arg=/INCREMENTAL:NO");
    println!("cargo:rustc-cdylib-link-arg=/DYNAMICBASE");
    println!("cargo:rustc-cdylib-link-arg=/NXCOMPAT");
}
