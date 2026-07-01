//! Minimal ANSI coloring for CLI output. Colors are applied only when stdout is
//! a TTY and `NO_COLOR` is unset, so piped/redirected output stays clean and
//! machine-parseable. No external crate required.

use std::io::IsTerminal;
use std::sync::OnceLock;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn paint(code: &str, s: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn cyan(s: &str) -> String {
    paint("36", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}

/// A stable color for a node kind, to make scanning `query`/`status` easier.
pub fn kind(kind: &str) -> String {
    let colored = match kind {
        "function" | "method" => cyan(kind),
        "class" | "struct" | "interface" | "trait" | "enum" | "protocol" => green(kind),
        "route" => yellow(kind),
        "module" | "namespace" | "file" => dim(kind),
        _ => kind.to_string(),
    };
    format!("[{}]", colored)
}
