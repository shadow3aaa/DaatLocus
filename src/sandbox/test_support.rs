use std::io;

use daat_locus_sandbox::capabilities::{NativeSupport, native_support};

/// Only an explicitly absent capability may return from a native test. Keep
/// diagnostics outside libtest's capture, including when the test returns early.
pub(crate) fn native_sandbox_available() -> bool {
    static SUPPORT: std::sync::OnceLock<Result<NativeSupport, String>> = std::sync::OnceLock::new();
    match SUPPORT.get_or_init(|| native_support().map_err(|error| error.to_string())) {
        Ok(NativeSupport::Supported) => true,
        Ok(NativeSupport::Unsupported(reason)) => {
            use std::io::Write;
            writeln!(
                io::stderr(),
                "native sandbox test not run (unsupported): {reason}"
            )
            .expect("cannot report unsupported native sandbox");
            false
        }
        Err(error) => panic!("native sandbox capability probe failed: {error}"),
    }
}
