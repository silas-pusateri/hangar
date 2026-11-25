//! Terminal I/O: prompts that read stdin, and a progress printer.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::Mutex;

use super::args::Globals;

pub fn stdout_is_tty() -> bool {
    // SAFETY: isatty only reads the file descriptor state.
    unsafe { libc::isatty(1) == 1 }
}

fn stdin_is_tty() -> bool {
    // SAFETY: isatty only reads the file descriptor state.
    unsafe { libc::isatty(0) == 1 }
}

/// Write text to stdout as is. Errors (for example a closed pipe) are ignored.
pub fn write_out(s: &str) {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(s.as_bytes());
    let _ = o.flush();
}

/// One line from stdin without the line end. None on end of input or a read error.
pub fn read_line() -> Option<String> {
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            // Piped answers are not echoed; end the prompt line so output stays readable.
            if !stdin_is_tty() {
                write_out("\n");
            }
            Some(s.trim_end_matches(['\n', '\r']).to_string())
        }
    }
}

/// Ask a yes/no question. `-y` and `--headless` answer yes. End of input answers no.
pub fn confirm(g: &Globals, question: &str) -> bool {
    if g.yes || g.headless {
        return true;
    }
    loop {
        write_out(&format!("{question} [Y/n] "));
        let Some(a) = read_line() else {
            write_out("\n");
            return false;
        };
        match a.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return true,
            "n" | "no" => return false,
            _ => write_out("Please answer y or n.\n"),
        }
    }
}

/// Ask a yes/no question that `-y` and `--headless` do not answer (for actions on the user's
/// own files). Returns false when the run is not interactive.
pub fn confirm_always_ask(g: &Globals, question: &str) -> bool {
    if g.headless {
        return false;
    }
    write_out(&format!("{question} [y/N] "));
    match read_line() {
        Some(a) => matches!(a.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        None => {
            // End of input: end the prompt line.
            write_out("\n");
            false
        }
    }
}

/// Ask for one number in 1..=n. None if cancelled (empty line, end of input).
pub fn choose_number(prompt: &str, n: usize) -> Option<usize> {
    loop {
        write_out(&format!("{prompt} [1-{n}, Enter to cancel]: "));
        let a = read_line()?;
        let a = a.trim();
        if a.is_empty() {
            return None;
        }
        match a.parse::<usize>() {
            Ok(k) if (1..=n).contains(&k) => return Some(k - 1),
            _ => write_out("Please enter a number from the list.\n"),
        }
    }
}

/// Parse a multi-choice answer: numbers (space or comma separated, ranges `2-4`), `a` = all,
/// empty = none. Returns 0-based indices, sorted, no duplicates. Err for bad input.
pub fn parse_multi_choice(answer: &str, n: usize) -> Result<Vec<usize>, String> {
    let a = answer.trim();
    if a.is_empty() {
        return Ok(Vec::new());
    }
    if a.eq_ignore_ascii_case("a") || a.eq_ignore_ascii_case("all") {
        return Ok((0..n).collect());
    }
    let mut out = Vec::new();
    for part in a.split([',', ' ']).filter(|p| !p.is_empty()) {
        let (lo, hi) = match part.split_once('-') {
            Some((x, y)) => (x, y),
            None => (part, part),
        };
        let lo: usize = lo.trim().parse().map_err(|_| format!("not a number: {part}"))?;
        let hi: usize = hi.trim().parse().map_err(|_| format!("not a number: {part}"))?;
        if lo == 0 || hi > n || lo > hi {
            return Err(format!("out of range: {part}"));
        }
        out.extend((lo - 1)..hi);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Prints progress: on a TTY one line rewritten with `\r`; otherwise one line per new message.
pub struct ProgressPrinter {
    tty: bool,
    state: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
    msg: String,
    pct: Option<u32>,
    /// A `\r` line is open and needs a newline at the end.
    open: bool,
    /// Messages already printed (digits removed), for output that is not a TTY.
    printed: HashSet<String>,
}

/// Columns of the terminal on stdout (80 if unknown).
fn terminal_columns() -> usize {
    // SAFETY: TIOCGWINSZ fills a winsize struct that we own.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 { usize::from(ws.ws_col) } else { 80 }
}

/// `msg` cut to at most `width` characters; a cut line ends with "...".
pub fn fit_width(msg: &str, width: usize) -> String {
    if msg.chars().count() <= width {
        return msg.to_string();
    }
    let keep = width.saturating_sub(3);
    let mut out: String = msg.chars().take(keep).collect();
    out.push_str(&".".repeat(width.min(3)));
    out
}

/// A message with its digits removed: "Downloading 2.0 MiB" and "Downloading 3.0 MiB" match.
pub fn message_shape(msg: &str) -> String {
    msg.chars().filter(|c| !c.is_ascii_digit()).collect()
}

impl ProgressPrinter {
    pub fn new() -> Self {
        ProgressPrinter { tty: stdout_is_tty(), state: Mutex::new(ProgressState::default()) }
    }

    pub fn update(&self, msg: &str, frac: Option<f32>) {
        let Ok(mut st) = self.state.lock() else { return };
        let pct = frac.map(|f| (f.clamp(0.0, 1.0) * 100.0) as u32);
        if st.msg == msg && st.pct == pct {
            return;
        }
        let new_msg = st.msg != msg;
        st.msg.clear();
        st.msg.push_str(msg);
        st.pct = pct;
        if self.tty {
            let tail = pct.map(|p| format!(" {p:>3}%")).unwrap_or_default();
            // A line wider than the terminal wraps, and `\r` cannot clear the upper part.
            let fit = fit_width(msg, terminal_columns().saturating_sub(1 + tail.chars().count()));
            write_out(&format!("\r\x1b[K{fit}{tail}"));
            st.open = true;
        } else if new_msg && !msg.is_empty() && st.printed.insert(message_shape(msg)) {
            write_out(&format!("{msg}\n"));
        }
    }

    /// End an open `\r` line.
    pub fn finish(&self) {
        let Ok(mut st) = self.state.lock() else { return };
        if st.open {
            write_out("\n");
            st.open = false;
        }
        st.msg.clear();
        st.pct = None;
        st.printed.clear();
    }
}
