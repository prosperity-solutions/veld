use std::io::{self, IsTerminal};

// ---------------------------------------------------------------------------
// ANSI helpers
// ---------------------------------------------------------------------------

pub fn is_tty() -> bool {
    io::stdout().is_terminal()
}

fn ansi(code: &str, text: &str) -> String {
    if is_tty() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn green(text: &str) -> String {
    ansi("32", text)
}

pub fn red(text: &str) -> String {
    ansi("31", text)
}

pub fn bold(text: &str) -> String {
    ansi("1", text)
}

pub fn dim(text: &str) -> String {
    ansi("2", text)
}

pub fn cyan(text: &str) -> String {
    ansi("36", text)
}

pub fn yellow(text: &str) -> String {
    ansi("33", text)
}

// ---------------------------------------------------------------------------
// Semantic helpers
// ---------------------------------------------------------------------------

pub fn checkmark() -> String {
    green("\u{2713}")
}

pub fn cross() -> String {
    red("\u{2717}")
}

/// Print a setup-style progress step, e.g. `[1/6] Checking ports...`
pub fn step(current: usize, total: usize, label: &str) -> String {
    let prefix = format!("[{}/{}]", current, total);
    format!("{} {}", bold(&prefix), label)
}

/// Right-pad `s` to `width` characters.
pub fn pad_right(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - s.len()))
    }
}

// ---------------------------------------------------------------------------
// Table helpers
// ---------------------------------------------------------------------------

/// Print a simple table with a header row. Each row is a `Vec<String>`.
pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if rows.is_empty() {
        return;
    }

    // Compute column widths.
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < cols {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }

    // Header
    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| bold(&pad_right(h, widths[i])))
        .collect();
    println!("{}", header_line.join("  "));

    // Separator
    let sep: Vec<String> = widths.iter().map(|&w| "-".repeat(w)).collect();
    println!("{}", dim(&sep.join("  ")));

    // Rows
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let w = widths.get(i).copied().unwrap_or(0);
                pad_right(c, w)
            })
            .collect();
        println!("{}", cells.join("  "));
    }
}

// ---------------------------------------------------------------------------
// JSON / error helpers
// ---------------------------------------------------------------------------

pub fn json_error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

/// Print a user-facing error. In JSON mode output structured JSON, otherwise
/// print a red message to stderr.
pub fn print_error(msg: &str, json: bool) {
    if json {
        println!("{}", json_error(msg));
    } else {
        eprintln!("{} {}", cross(), red(msg));
    }
}

/// Print a user-facing success line.
pub fn print_success(msg: &str) {
    println!("{} {}", checkmark(), green(msg));
}

/// Print a user-facing info line.
pub fn print_info(msg: &str) {
    println!("{}", msg);
}
