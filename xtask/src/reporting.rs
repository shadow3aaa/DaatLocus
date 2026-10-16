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
