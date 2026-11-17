// vim: ts=4 et

//! Generic "run text through a pager" helper, kept separate from
//! `man.rs` so any future long-output feature can reuse it without
//! pulling in roff-formatting logic. Mirrors `__template.py`'s Pager
//! class: if standard output is not a terminal, write the text to our
//! own stdout untouched; otherwise resolve `$PAGER` (treated as a
//! whole shell command line, since it may be e.g. "less -R") or,
//! failing that, the first of less/more/pg found in `$PATH`. If no
//! pager can be resolved at all, falls back to writing to our own
//! stdout.

use std::io::{self, IsTerminal, Write};
use std::process::{Command, Stdio};

const FALLBACK_PAGERS: [&str; 3] = ["less", "more", "pg"];

/// Writes `text` to stdout, through a pager if stdout is a terminal.
pub fn page(text: &str) {
    if io::stdout().is_terminal() && run_pager(text) {
        return;
    }

    let _ = io::stdout().write_all(text.as_bytes());
}

fn run_pager(text: &str) -> bool {
    if let Ok(cmd) = std::env::var("PAGER")
        && !cmd.is_empty()
        && pipe_through("/bin/sh", &["-c", &cmd], text)
    {
        return true;
    }

    FALLBACK_PAGERS
        .iter()
        .any(|candidate| pipe_through(candidate, &[], text))
}

/// Runs `program args...`, feeding `text` to its stdin and letting its
/// stdout/stderr fall through to ours. Returns false (having written
/// nothing of our own) if the program can't even be spawned, e.g. not
/// found in `$PATH`, so the caller can fall back to something else.
fn pipe_through(program: &str, args: &[&str], text: &str) -> bool {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .spawn();

    let mut child = match child {
        Ok(child) => child,
        Err(_) => return false,
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }

    let _ = child.wait();
    true
}
