use crate::Result;
use std::{
    io::{self, IsTerminal},
    path::Path,
};

pub fn status(label: &str, message: &str) {
    println!("{} {message}", status_label(label, styled()));
}

fn styled() -> bool {
    io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").as_deref() != Ok("dumb")
}

fn status_label(label: &str, color: bool) -> String {
    if color {
        format!("\x1b[1;92m{label:>12}\x1b[0m")
    } else {
        format!("{label:>12}")
    }
}

pub fn packaged(label: &str, path: &Path) -> Result<()> {
    // Canonicalization also ensures we never announce an artifact that is missing.
    let path = path.canonicalize()?;
    let url = reqwest::Url::from_file_path(&path).map_err(|_| "Invalid artifact path")?;
    let link = if styled() {
        format!("\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\")
    } else {
        url.to_string()
    };
    status("Packaged", &format!("{label} at {link}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redirected_status_is_aligned_and_has_no_escape_sequences() {
        assert_eq!(status_label("Packaged", false), "    Packaged");
        assert!(status_label("Packaged", true).contains("\x1b[1;92m"));
    }
    #[test]
    fn artifact_urls_escape_spaces_unicode_and_fragment_characters() {
        let path = std::env::temp_dir().join("打包 #1.apk");
        let url = reqwest::Url::from_file_path(&path).unwrap();
        assert!(url.as_str().contains("%20%231.apk"));
        assert_eq!(url.to_file_path().unwrap(), path);
    }
}
