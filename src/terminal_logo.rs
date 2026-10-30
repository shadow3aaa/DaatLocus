const PIXELS: &str = include_str!("../assets/wordmark.txt");
const RESET: &str = "\x1b[0m";

pub fn help_template() -> String {
    // clap reflows before_help as prose, including whitespace inside pixel art.
    // A literal template prefix preserves the logo while ordinary help still wraps.
    format!(
        "{}\n\n{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{{all-args}}{{after-help}}",
        render_daat_locus_logo()
    )
}

pub fn render_daat_locus_logo() -> String {
    let rows: Vec<_> = PIXELS.lines().map(str::as_bytes).collect();
    let mut out = String::new();
    for (index, pair) in rows.chunks(2).enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let mut style = (b'.', false);
        for (column, &top) in pair[0].iter().enumerate() {
            let bottom = pair.get(1).map_or(b'.', |row| row[column]);
            let top_ink = matches!(top, b'C' | b'V');
            let bottom_ink = matches!(bottom, b'C' | b'V');
            let ink = if top_ink {
                top
            } else if bottom_ink {
                bottom
            } else {
                b'.'
            };
            let shadow = top == b'S' || bottom == b'S';
            if style != (ink, shadow) {
                out.push_str(RESET);
                match ink {
                    b'C' => out.push_str("\x1b[38;2;183;177;177m"),
                    b'V' => out.push_str("\x1b[38;2;241;236;236m"),
                    _ => {}
                }
                if shadow {
                    out.push_str("\x1b[48;2;75;70;70m");
                }
                style = (ink, shadow);
            }
            out.push(match (top_ink, bottom_ink) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push_str(RESET);
    }
    out
}
