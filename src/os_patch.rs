//! Compile platform guidance independently from Actor role/project instructions.
pub(crate) fn for_os(os: &str) -> Option<&'static str> {
    let patch = match os {
        "windows" => include_str!("app_instructions/os/windows-patch.md"),
        "linux" => include_str!("app_instructions/os/linux-patch.md"),
        "macos" => include_str!("app_instructions/os/macos-patch.md"),
        "android" => include_str!("app_instructions/os/android-patch.md"),
        _ => "",
    };
    (!patch.trim().is_empty()).then_some(patch)
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_current_nonempty_platform_guidance_is_selected() {
        assert!(super::for_os("windows").unwrap().contains("MSIX"));
        for os in ["linux", "macos", "android", "unknown"] {
            assert!(
                super::for_os(os).is_none(),
                "empty/unknown patches must not add prompt material"
            );
        }
    }
}
