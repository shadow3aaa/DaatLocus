#[path = "build_support/development_certificate.rs"]
mod development_certificate;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/development_certificate.rs");
    println!("cargo:rerun-if-env-changed=DAAT_LOCUS_DEVELOPMENT_CERTIFICATE");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var_os("CARGO_FEATURE_DRIVER_SETUP").is_none()
    {
        return;
    }
    let source = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let certificate = if std::env::var("PROFILE").as_deref() == Ok("debug") {
        match std::env::var_os("DAAT_LOCUS_DEVELOPMENT_CERTIFICATE") {
            Some(path) => std::path::PathBuf::from(path),
            None => {
                let identity =
                    development_certificate::ensure(source.parent().unwrap().parent().unwrap())
                        .expect("prepare local development signing certificate");
                eprintln!(
                    "Debug display helper uses local development certificate {}",
                    identity.thumbprint
                );
                identity.certificate
            }
        }
    } else {
        source.join("certificates/virtual-display.cer")
    };
    let certificate = certificate
        .canonicalize()
        .expect("display signing certificate");
    println!("cargo:rerun-if-changed={}", certificate.display());
    println!(
        "cargo:rustc-env=DL_DISPLAY_CERTIFICATE={}",
        certificate.display()
    );
}
