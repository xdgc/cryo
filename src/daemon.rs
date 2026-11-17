// vim: ts=4 et

//! The long-running mode from cryo.md, "Execution model": keeps the
//! discovered model in memory, wakes on a self-computed timer to act
//! on whatever's due, and answers the local control channel (ipc.rs)
//! for `status`/`run`/`resync`/`stop` from any other `cryo`
//! invocation. Also best-effort watches `zpool events -f` to resync
//! early on a `zfs set`/dataset change, rather than waiting out the
//! timer -- but that's purely a responsiveness optimization; the
//! timer-driven sweep below is what's actually relied on.
//!
//! This runs in the foreground, as a single process with no
//! double-fork/setsid detachment -- whatever supervises it (a service
//! manager, or a terminal for now) owns backgrounding it.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::discover::{self, Model};
use crate::ipc;
use crate::report;
use crate::snapshot;
use crate::timestamp::Timestamp;

/// Upper bound on how long the main loop ever sleeps: both the
/// safety-net resync interval, and (via `next_wake`) the longest it'll
/// go without rechecking what's due, even if nothing is.
const PERIODIC_RESYNC_INTERVAL: Duration = Duration::from_secs(300);

/// A burst of `zpool events` shouldn't trigger a resync per line --
/// collapse anything arriving within this long of the last resync.
const EVENT_DEBOUNCE: Duration = Duration::from_secs(5);

type Reply = Sender<Result<String, String>>;

enum Message {
    Status { reply: Reply },
    Run { reply: Reply },
    Resync { reply: Reply },
    Stop { reply: Reply },
    EventHint,
    /// A background resync (see `spawn_resync`) has finished. Carries
    /// the result rather than mutating `model` itself, since it runs
    /// on its own thread -- only the main loop touches `model`.
    ResyncDone(Result<Model, String>),
}

pub fn run(prog_name: &str) -> ExitCode {
    let socket_path = ipc::socket_path();

    if UnixStream::connect(&socket_path).is_ok() {
        eprintln!(
            "{prog_name}: a daemon is already listening on {}",
            socket_path.display()
        );
        return ExitCode::FAILURE;
    }

    // Discover before binding the socket, not after: an initial scan
    // can take tens of seconds on a host with enough datasets (see
    // cryo.md's noted 26s/665-dataset finding), and a request that
    // connects successfully but then has nobody accepting it yet
    // would just hang instead of cleanly falling back. With no socket
    // present until discovery's done, an impatient caller gets a
    // clean "unavailable" and falls back to scanning itself instead.
    let mut model = match discover::discover() {
        Ok(model) => model,
        Err(e) => {
            eprintln!("{prog_name}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Either nothing's there, or it's a stale socket file left by a
    // daemon that didn't shut down cleanly (e.g. killed outright).
    let _ = std::fs::remove_file(&socket_path);

    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("{prog_name}: binding {}: {e}", socket_path.display());
            return ExitCode::FAILURE;
        }
    };

    let (tx, rx) = mpsc::channel();
    spawn_listener(listener, tx.clone());
    spawn_event_watcher(tx.clone());

    eprintln!("{prog_name}: listening on {}", socket_path.display());
    let mut last_resync = Instant::now();

    // A resync runs on its own thread (it's the slow, whole-pool-scan
    // operation -- see cryo.md's 26s/665-dataset finding) so it never
    // blocks this loop from answering other requests or acting on
    // what's due meanwhile. At most one runs at a time; anyone asking
    // for a resync while one's already in flight gets the result of
    // that one once it lands, rather than queuing up a redundant scan.
    let mut resync_in_flight = false;
    let mut pending_resyncs: Vec<Reply> = Vec::new();

    loop {
        let now = Timestamp::now();
        let timeout = next_wake(&model, now);

        match rx.recv_timeout(timeout) {
            Ok(Message::Status { reply }) => {
                let _ = reply.send(Ok(report::render_status(&model, Timestamp::now())));
            }
            Ok(Message::Run { reply }) => {
                let result = snapshot::apply_due_actions(&mut model, Timestamp::now());
                let body = report::render_action_result(&result);
                if result.errors.is_empty() {
                    let _ = reply.send(Ok(body));
                } else {
                    let _ = reply.send(Err(result.errors.join("; ")));
                }
            }
            Ok(Message::Resync { reply }) => {
                pending_resyncs.push(reply);
                if !resync_in_flight {
                    resync_in_flight = true;
                    spawn_resync(tx.clone());
                }
            }
            Ok(Message::Stop { reply }) => {
                let _ = reply.send(Ok(String::new()));
                break;
            }
            Ok(Message::EventHint) => {
                if !resync_in_flight && last_resync.elapsed() >= EVENT_DEBOUNCE {
                    resync_in_flight = true;
                    spawn_resync(tx.clone());
                }
            }
            Ok(Message::ResyncDone(outcome)) => {
                resync_in_flight = false;
                last_resync = Instant::now();

                let reply_outcome = match outcome {
                    Ok(fresh) => {
                        model = fresh;
                        Ok(String::new())
                    }
                    Err(e) => Err(e),
                };

                for reply in pending_resyncs.drain(..) {
                    let _ = reply.send(reply_outcome.clone());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let result = snapshot::apply_due_actions(&mut model, Timestamp::now());
                for e in &result.errors {
                    eprintln!("{prog_name}: {e}");
                }

                if !resync_in_flight && last_resync.elapsed() >= PERIODIC_RESYNC_INTERVAL {
                    resync_in_flight = true;
                    spawn_resync(tx.clone());
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    ExitCode::SUCCESS
}

fn spawn_resync(tx: Sender<Message>) {
    thread::spawn(move || {
        let outcome = discover::discover().map_err(|e| e.to_string());
        let _ = tx.send(Message::ResyncDone(outcome));
    });
}

/// How long to sleep before the next due action, capped by the
/// periodic safety-net interval so the loop always wakes that often
/// regardless -- both to resync on schedule, and so a dataset with no
/// policy due for months doesn't starve everything else of attention.
fn next_wake(model: &Model, now: Timestamp) -> Duration {
    let mut earliest = PERIODIC_RESYNC_INTERVAL.as_secs() as i64;

    for state in model.datasets.values() {
        for p in &state.policies {
            let remaining = match state.last_fired.get(&p.name) {
                None => 0,
                Some(last) => {
                    let elapsed = last.elapsed_secs_until(now);
                    (p.frequency.minutes() as i64 * 60 - elapsed).max(0)
                }
            };
            earliest = earliest.min(remaining);
        }
    }

    Duration::from_secs(earliest.max(0) as u64)
}

fn spawn_listener(listener: UnixListener, tx: Sender<Message>) {
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let tx = tx.clone();
            thread::spawn(move || handle_connection(stream, &tx));
        }
    });
}

fn handle_connection(mut stream: UnixStream, tx: &Sender<Message>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });

    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }

    let (reply, reply_rx) = mpsc::channel();
    let sent = match line.trim() {
        "STATUS" => tx.send(Message::Status { reply }),
        "RUN" => tx.send(Message::Run { reply }),
        "RESYNC" => tx.send(Message::Resync { reply }),
        "STOP" => tx.send(Message::Stop { reply }),
        other => {
            let _ = write_response(&mut stream, &Err(format!("unknown request '{other}'")));
            return;
        }
    };

    if sent.is_err() {
        return; // the main loop is gone; nothing more we can do
    }

    if let Ok(result) = reply_rx.recv() {
        let _ = write_response(&mut stream, &result);
    }
}

fn write_response(stream: &mut UnixStream, result: &Result<String, String>) -> std::io::Result<()> {
    match result {
        Ok(body) => write!(stream, "OK\n{body}"),
        Err(msg) => writeln!(stream, "ERR {msg}"),
    }
}

/// Best-effort: if `zpool events -f` can't be spawned (missing
/// permission, no pools, whatever), the daemon still works fine on
/// the timer alone -- this only ever shortens the wait.
fn spawn_event_watcher(tx: Sender<Message>) {
    thread::spawn(move || watch_events(&tx));
}

fn watch_events(tx: &Sender<Message>) {
    let child = Command::new("zpool")
        .args(["events", "-f"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let Ok(mut child) = child else { return };
    let Some(stdout) = child.stdout.take() else { return };

    for line in BufReader::new(stdout).lines() {
        if line.is_err() || tx.send(Message::EventHint).is_err() {
            break;
        }
    }

    let _ = child.wait();
}
