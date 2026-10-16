const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy)]
struct LogoTone {
    ink: &'static str,
    shadow_fg: &'static str,
    shadow_bg: &'static str,
}

const LEFT_TONE: LogoTone = LogoTone {
    ink: "\x1b[38;5;87m",
    shadow_fg: "\x1b[38;5;24m",
    shadow_bg: "\x1b[48;5;24m",
};

const RIGHT_TONE: LogoTone = LogoTone {
    ink: "\x1b[38;5;141m",
    shadow_fg: "\x1b[38;5;54m",
    shadow_bg: "\x1b[48;5;54m",
};

const DAAT_LEFT: &[&str] = &[
    "                   ",
    "█▀▀▄ █▀▀█ █▀▀█ ▀█▀ ",
    "█__█ █__█ █__█  █  ",
    "▀▀▀  ▀  ▀ ▀  ▀  ▀  ",
];

const LOCUS_RIGHT: &[&str] = &[
    "                        ",
    "█    █▀▀█ █▀▀▀ █  █ █▀▀▀",
    "█    █__█ █___ █__█ ▀▀▀█",
    "▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀ ▀▀▀▀",
];

pub fn render_daat_locus_logo() -> String {
    let mut out = String::new();
    let rows = DAAT_LEFT.len().max(LOCUS_RIGHT.len());

    for row in 0..rows {
        if row > 0 {
            out.push('\n');
        }

        render_template_line(
            &mut out,
            DAAT_LEFT.get(row).copied().unwrap_or_default(),
            LEFT_TONE,
        );
        out.push_str("   ");
        render_template_line(
            &mut out,
            LOCUS_RIGHT.get(row).copied().unwrap_or_default(),
            RIGHT_TONE,
        );
    }

    out
}

fn render_template_line(out: &mut String, line: &str, tone: LogoTone) {
    for ch in line.chars() {
        match ch {
            ' ' => out.push(' '),
            '_' => {
                out.push_str(tone.shadow_bg);
                out.push(' ');
                out.push_str(RESET);
            }
            '^' => {
                out.push_str(tone.ink);
                out.push_str(tone.shadow_bg);
                out.push('▀');
                out.push_str(RESET);
            }
            '~' => {
                out.push_str(tone.shadow_fg);
                out.push('▀');
                out.push_str(RESET);
            }
            ',' => {
                out.push_str(tone.shadow_fg);
                out.push('▄');
                out.push_str(RESET);
            }
            '█' | '▀' | '▄' => {
                out.push_str(tone.ink);
                out.push(ch);
                out.push_str(RESET);
            }
            _ => out.push(ch),
        }
    }
}
