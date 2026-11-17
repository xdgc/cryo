// vim: ts=4 et

//! CRYO -- zfs snapshot lifecycle maintenance and replication.
//!
//! See cryo.md for the design and man/cryo.1 for the user-facing
//! manual page (embedded via man.rs). `man.rs` and `pager.rs` are
//! used as-is, unedited, from the starter template.

mod daemon;
mod discover;
mod error;
mod handshake;
mod ipc;
mod man;
mod origin;
mod pager;
mod policy;
mod protocol;
mod replicate;
mod report;
mod server;
mod size;
mod snapname;
mod snapshot;
mod span;
mod stream;
mod sysinfo;
mod target;
mod timestamp;
mod transport;
mod zfs;

use std::env;
use std::process::ExitCode;

use timestamp::Timestamp;

const PROGRAM_NAME: &str = "cryo";
const PROGRAM_VERSION: &str = "0.1.0";

#[derive(Default)]
struct Options {
    debug: bool,
}

enum Subcommand {
    Daemon,
    Run,
    Server,
    Status,
    Resync,
    Stop,
    /// One-shot replication (cryo.md, "Replication behaviors" (l)):
    /// `dataset` and `target` are given directly on the command line
    /// rather than read from a `cryo:target*` property.
    Replicate { dataset: String, target_spec: String },
}

/// `Proceed` carries the subcommand and options the program should act
/// on. `Handled` means --help/--version/--man already printed
/// everything there is to do (the caller should exit 0). `UsageError`
/// means an unknown argument, or no/unknown subcommand, was rejected (a
/// message has already gone to stderr; the caller should exit nonzero).
enum ParseOutcome {
    Proceed(Options, Subcommand),
    Handled,
    UsageError,
}

fn print_usage(prog_name: &str) {
    eprintln!("usage: {prog_name} [options] <daemon|run|server|status|resync|stop>");
    eprintln!("       {prog_name} [options] replicate <dataset> <target>");
}

fn print_help(prog_name: &str) {
    println!("usage: {prog_name} [options] <subcommand>");
    println!("       {prog_name} [options] replicate <dataset> <target>");
    println!();
    println!("zfs snapshot lifecycle maintenance and replication.");
    println!();
    println!("subcommands:");
    println!("  daemon      run continuously, scheduling snapshot/expiry actions");
    println!("  run         perform whatever is due right now, then exit");
    println!("  server      speak the cryo wire protocol on stdin/stdout");
    println!("  status      report policies, schedules, and claims");
    println!("  resync      ask a running daemon to re-scan zfs immediately");
    println!("  stop        ask a running daemon to shut down cleanly");
    println!("  replicate   one-shot replication of <dataset> to <target>, where");
    println!("              <target> uses the same grammar as a cryo:target value");
    println!();
    println!("run/status prefer a running daemon's cached state, falling back to");
    println!("scanning zfs directly if none is reachable; resync/stop require a");
    println!("running daemon and fail if none is reachable.");
    println!();
    println!("options:");
    println!("  -d, --debug     enable debugging");
    println!("      --man       show the full manual page");
    println!("  -V, --version   print version and exit");
    println!("  -h, --help      print this help and exit");
}

fn print_version() {
    println!("{PROGRAM_NAME} {PROGRAM_VERSION}");
}

fn parse_subcommand(prog_name: &str, name: &str) -> Option<Subcommand> {
    match name {
        "daemon" => Some(Subcommand::Daemon),
        "run" => Some(Subcommand::Run),
        "server" => Some(Subcommand::Server),
        "status" => Some(Subcommand::Status),
        "resync" => Some(Subcommand::Resync),
        "stop" => Some(Subcommand::Stop),
        _ => {
            eprintln!("{prog_name}: unknown subcommand {name}");
            None
        }
    }
}

fn parse_args(prog_name: &str, args: &[String]) -> ParseOutcome {
    let mut opts = Options::default();
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-d" | "--debug" => {
                opts.debug = true;
                i += 1;
            }
            "--man" => {
                man::show();
                return ParseOutcome::Handled;
            }
            "-V" | "--version" => {
                print_version();
                return ParseOutcome::Handled;
            }
            "-h" | "--help" => {
                print_help(prog_name);
                return ParseOutcome::Handled;
            }
            "replicate" => {
                let rest = &args[i + 1..];
                if rest.len() != 2 {
                    eprintln!("{prog_name}: replicate requires exactly <dataset> <target>");
                    print_usage(prog_name);
                    return ParseOutcome::UsageError;
                }
                let subcommand = Subcommand::Replicate {
                    dataset: rest[0].clone(),
                    target_spec: rest[1].clone(),
                };
                return ParseOutcome::Proceed(opts, subcommand);
            }
            arg => {
                return match parse_subcommand(prog_name, arg) {
                    Some(subcommand) if i == args.len() - 1 => {
                        ParseOutcome::Proceed(opts, subcommand)
                    }
                    Some(_) => {
                        eprintln!("{prog_name}: unexpected arguments after subcommand");
                        print_usage(prog_name);
                        ParseOutcome::UsageError
                    }
                    None => {
                        print_usage(prog_name);
                        ParseOutcome::UsageError
                    }
                };
            }
        }
    }

    eprintln!("{prog_name}: no subcommand given");
    print_usage(prog_name);
    ParseOutcome::UsageError
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let prog_name = args.first().map_or(PROGRAM_NAME, String::as_str);

    let (opts, subcommand) = match parse_args(prog_name, &args[1..]) {
        ParseOutcome::Proceed(opts, subcommand) => (opts, subcommand),
        ParseOutcome::Handled => return ExitCode::SUCCESS,
        ParseOutcome::UsageError => return ExitCode::FAILURE,
    };

    if opts.debug {
        eprintln!("{prog_name}: debugging enabled");
    }

    match subcommand {
        Subcommand::Daemon => daemon::run(prog_name),
        Subcommand::Run => cmd_run(prog_name),
        Subcommand::Server => server::run(),
        Subcommand::Status => cmd_status(prog_name),
        Subcommand::Resync => cmd_daemon_only(prog_name, ipc::Request::Resync),
        Subcommand::Stop => cmd_daemon_only(prog_name, ipc::Request::Stop),
        Subcommand::Replicate { dataset, target_spec } => cmd_replicate(prog_name, &dataset, &target_spec),
    }
}

/// One-shot replication, cryo.md "Replication behaviors" (l).
fn cmd_replicate(prog_name: &str, dataset: &str, target_spec: &str) -> ExitCode {
    let target = match target::parse_target_value("", target_spec) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("{prog_name}: {e}");
            return ExitCode::FAILURE;
        }
    };

    match replicate::one_shot(dataset, &target) {
        Ok(outcome) => {
            println!("{}", outcome.message);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{prog_name}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reports every discovered policy and cryo-managed snapshot, with
/// its claims and next-due time -- a read-only view of the daemon's
/// discovery/scheduling logic, useful on its own and as a sanity check
/// that the property parser sees what you expect it to. Prefers a
/// running daemon's cached state; falls back to scanning zfs directly
/// if none is reachable.
fn cmd_status(prog_name: &str) -> ExitCode {
    match ipc::request(ipc::Request::Status) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(ipc::Error::Failed(msg)) => {
            eprintln!("{prog_name}: {msg}");
            ExitCode::FAILURE
        }
        Err(ipc::Error::Unavailable) => {
            let model = match discover::discover() {
                Ok(model) => model,
                Err(e) => {
                    eprintln!("{prog_name}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            print!("{}", report::render_status(&model, Timestamp::now()));
            ExitCode::SUCCESS
        }
    }
}

/// Takes whatever snapshots are due and expires whatever claims have
/// lapsed, across every discovered dataset, then exits -- the `cron`
/// fallback mode from cryo.md, "Execution model". Prefers asking a
/// running daemon to do this against its own cached state; falls back
/// to scanning and acting directly if none is reachable.
fn cmd_run(prog_name: &str) -> ExitCode {
    match ipc::request(ipc::Request::Run) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(ipc::Error::Failed(msg)) => {
            eprintln!("{prog_name}: {msg}");
            ExitCode::FAILURE
        }
        Err(ipc::Error::Unavailable) => {
            let mut model = match discover::discover() {
                Ok(model) => model,
                Err(e) => {
                    eprintln!("{prog_name}: {e}");
                    return ExitCode::FAILURE;
                }
            };

            let result = snapshot::apply_due_actions(&mut model, Timestamp::now());
            print!("{}", report::render_action_result(&result));

            for e in &result.errors {
                eprintln!("{prog_name}: {e}");
            }

            if result.errors.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// `resync`/`stop`: these only mean anything against a running
/// daemon, so unlike `run`/`status` there's no standalone fallback.
fn cmd_daemon_only(prog_name: &str, req: ipc::Request) -> ExitCode {
    match ipc::request(req) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(ipc::Error::Failed(msg)) => {
            eprintln!("{prog_name}: {msg}");
            ExitCode::FAILURE
        }
        Err(ipc::Error::Unavailable) => {
            eprintln!("{prog_name}: no daemon running");
            ExitCode::FAILURE
        }
    }
}

// Requirements this program was built to satisfy:
//
// - Maintain zfs snapshot lifecycle (periodic/ad-hoc snapshots, named
//   retention policies, expiry) purely from cryo:* zfs user properties,
//   with no configuration files.
// - Replicate cryo-managed snapshots to local or remote targets over
//   ssh, a pipe transport, or locally, via a framed binary protocol
//   that supports indefinite-length streaming within a persistent,
//   multi-operation session.
// - Run as a long-running daemon by default, with a cron-style one-shot
//   fallback mode.
// - See cryo.md for the full design.
