// vim: ts=4 et

//! Implements --man: if standard output is a terminal, formats the
//! embedded man page (see `man/__template.1`, embedded verbatim via
//! `include_str!` -- there is no `gen-man-header`-equivalent code-gen
//! step the way the C starter needs one, since Rust string literals
//! have no practical length limit the way C99's 4095-character limit
//! forced chunking) through whichever of groff/the classic
//! tbl|eqn|nroff|col pipeline/mandoc is actually installed, then hands
//! the result to `pager::page()`; otherwise writes the raw roff source
//! verbatim, so that "programname --man >programname.1" produces an
//! installable man page source file.

use std::io::{self, IsTerminal, Read, Write};
use std::process::{Child, Command, Stdio};

use crate::pager;

const MAN_PAGE: &str = include_str!("../man/cryo.1");

pub fn show() {
    if !io::stdout().is_terminal() {
        print!("{MAN_PAGE}");
        return;
    }

    match format(MAN_PAGE) {
        Some(formatted) => pager::page(&formatted),
        None => print!("{MAN_PAGE}"),
    }
}

/// Picks a roff formatter: groff in one shot if available (it folds
/// tbl/eqn preprocessing and the troff-to-terminal postprocessing into
/// one invocation via -t -e -p); otherwise the classic separate
/// tbl | eqn | nroff -man | col pipeline, using whichever of those
/// exist; otherwise mandoc, which (like groff) needs no separate
/// pre/post-processing stages. Returns None if nothing usable is found
/// in $PATH, so the caller can fall back to the raw source.
fn format(source: &str) -> Option<String> {
    if which("groff") {
        let device = groff_device();
        let stages = [vec!["groff", "-man", "-t", "-e", "-p", "-T", device]];
        return run_pipeline(&stages, source);
    }

    let mut stages: Vec<Vec<&str>> = Vec::new();
    let mut used_troff_family = false;

    if which("tbl") {
        stages.push(vec!["tbl"]);
    }

    if which("eqn") {
        stages.push(vec!["eqn"]);
    }

    if which("nroff") {
        stages.push(vec!["nroff", "-man"]);
        used_troff_family = true;
    } else if which("troff") {
        stages.push(vec!["troff", "-man"]);
        used_troff_family = true;
    } else if which("mandoc") {
        stages.push(vec!["mandoc"]);
    }

    if stages.is_empty() {
        return None;
    }

    if used_troff_family && which("col") {
        stages.push(vec!["col", "-bx"]);
    }

    run_pipeline(&stages, source)
}

/// Wires up `stages` as a pipeline (stage i's stdout feeds stage i+1's
/// stdin), feeds `source` into the first stage's stdin from a
/// background thread (so a stage that only starts producing output
/// after consuming all its input, e.g. nroff, can't deadlock against
/// us), and collects the last stage's stdout.
fn run_pipeline(stages: &[Vec<&str>], source: &str) -> Option<String> {
    if stages.is_empty() {
        return None;
    }

    let mut children: Vec<Child> = Vec::with_capacity(stages.len());
    let mut prev_stdout: Option<std::process::ChildStdout> = None;
    let mut writer = None;

    for (i, stage) in stages.iter().enumerate() {
        let mut cmd = Command::new(stage[0]);
        cmd.args(&stage[1..]);
        cmd.stdin(match prev_stdout.take() {
            Some(stdout) => Stdio::from(stdout),
            None => Stdio::piped(),
        });
        cmd.stdout(Stdio::piped());

        let mut child = cmd.spawn().ok()?;
        prev_stdout = child.stdout.take();

        if i == 0 {
            let mut stdin = child.stdin.take()?;
            let text = source.to_string();
            writer = Some(std::thread::spawn(move || {
                let _ = stdin.write_all(text.as_bytes());
            }));
        }

        children.push(child);
    }

    let mut output = String::new();

    if let Some(mut stdout) = prev_stdout {
        let _ = stdout.read_to_string(&mut output);
    }

    if let Some(writer) = writer {
        let _ = writer.join();
    }

    for mut child in children {
        let _ = child.wait();
    }

    Some(output)
}

/// Searches $PATH for an executable named `name`, the same way the C
/// starter's findInPath() does (rather than letting execvp() search
/// implicitly), so a missing stage is discovered before anything is
/// spawned.
fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(name))
                .any(|candidate| candidate.is_file() && is_executable(&candidate))
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    true
}

fn groff_device() -> &'static str {
    for var in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(value) = std::env::var(var)
            && (value.contains("UTF-8") || value.contains("utf8") || value.contains("UTF8"))
        {
            return "utf8";
        }
    }

    "ascii"
}
