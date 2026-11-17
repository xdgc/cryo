// vim: ts=4 et

//! `cryo server`: the receiving side of a replication session,
//! speaking the wire protocol over its own stdin/stdout -- see
//! transport.rs for how the client side gets a process connected to
//! them (ssh, pipe, or local). See cryo.md, "Replication behaviors"
//! and "Wire protocol".

use std::io;
use std::process::ExitCode;

use crate::error::{CryoError, Result};
use crate::handshake::{self, Hello, HelloAck};
use crate::origin::{self, Origin};
use crate::snapname;
use crate::stream::{self, StreamOutcome};
use crate::sysinfo;
use crate::zfs;

pub fn run() -> ExitCode {
    match run_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cryo server: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner() -> Result<()> {
    let our_hostname = sysinfo::hostname()?;
    // Plain (unlocked) handles, not `.lock()`'d: `StdinLock` holds a
    // `MutexGuard` internally and isn't `Send`, but `receive_stream`
    // needs to move its reader onto a dedicated thread (see
    // stream.rs) to make heartbeat emission honest. `Stdin`/`Stdout`
    // are just handles to the same global buffered stream regardless
    // of how many of them exist, so each call still locks internally
    // per read/write -- there's no correctness difference here, only
    // the ability to send one across a thread boundary.
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    let hello = handshake::read_hello(&mut stdin)?;
    let claimed = Origin {
        hostname: hello.hostname.clone(),
        dataset_path: hello.dataset_path.clone(),
        pool_guid: hello.pool_guid,
        dataset_guid: hello.dataset_guid,
    };

    let exists = zfs::dataset_exists(&hello.destination_path)?;

    if exists
        && let Some(recorded) = existing_origin(&hello.destination_path)?
        && !recorded.matches(&claimed)
    {
        let message = format!(
            "{} is already a replica of {}|{} (guids {}/{}), not {}|{} (guids {}/{}) -- refusing to overwrite a different lineage",
            hello.destination_path,
            recorded.hostname,
            recorded.dataset_path,
            recorded.pool_guid,
            recorded.dataset_guid,
            claimed.hostname,
            claimed.dataset_path,
            claimed.pool_guid,
            claimed.dataset_guid,
        );
        handshake::reject(&mut stdout, &message)?;
        return Err(CryoError::Command(message));
    }

    let existing_snapshots = if exists { snapshot_names_of(&hello.destination_path)? } else { Vec::new() };
    let resume_token = if exists { zfs::resume_token(&hello.destination_path)? } else { None };

    handshake::ack(&mut stdout, &HelloAck { hostname: our_hostname, existing_snapshots, resume_token })?;

    if !handshake::read_proceed(&mut stdin)? {
        return Ok(()); // client has nothing new to send; nothing more to do
    }

    let mut receive = zfs::spawn_receive(&hello.destination_path)?;
    let mut receive_stdin = receive.stdin.take().expect("spawned with piped stdin");

    // `receive_stream` takes `stdin` by value (its reader thread owns
    // it for the duration) and hands it back on structural success,
    // since `read_check_request` below still needs it.
    let receive_outcome = stream::receive_stream(stdin, &mut receive_stdin, stream::STALL_TIMEOUT);
    drop(receive_stdin);
    let receive_status = receive.wait()?;

    let (mut stdin, outcome) = receive_outcome?;

    match outcome {
        StreamOutcome::Aborted => Err(CryoError::Command("sender aborted the stream".to_string())),
        StreamOutcome::Completed => {
            // Only on a clean END-OF-STREAM does the client follow up
            // with a check request -- see handshake.rs.
            handshake::read_check_request(&mut stdin)?;

            if !receive_status.success() {
                let message = format!("zfs receive exited with {receive_status}");
                handshake::respond_error(&mut stdout, &message)?;
                return Err(CryoError::Command(message));
            }

            finalize(&hello, &claimed)?;
            handshake::respond_ok(&mut stdout)?;
            Ok(())
        }
    }
}

fn existing_origin(dataset: &str) -> Result<Option<Origin>> {
    let value = zfs::get_property(dataset, origin::ORIGIN_PROPERTY)?;
    if value == "-" { Ok(None) } else { Ok(Some(Origin::parse(&value)?)) }
}

/// Bare (post-`@`) names of `dataset`'s own cryo-managed snapshots.
fn snapshot_names_of(dataset: &str) -> Result<Vec<String>> {
    Ok(zfs::list_snapshots(dataset)?
        .into_iter()
        .filter(|s| s.full_name.split_once('@').map(|(ds, _)| ds) == Some(dataset))
        .filter(|s| snapname::parse(&s.full_name).is_some())
        .filter_map(|s| s.full_name.split_once('@').map(|(_, name)| name.to_string()))
        .collect())
}

/// Sets `cryo:origin` on the destination dataset and on whichever
/// snapshot the receive just created (the newest one, by creation
/// time -- correct as long as nothing else is concurrently writing to
/// this dataset, which holds for cryo's own management of a replica).
fn finalize(hello: &Hello, claimed: &Origin) -> Result<()> {
    zfs::set_property(&hello.destination_path, origin::ORIGIN_PROPERTY, &claimed.format())?;

    let newest = zfs::list_snapshots(&hello.destination_path)?
        .into_iter()
        .filter(|s| s.full_name.split_once('@').map(|(ds, _)| ds) == Some(hello.destination_path.as_str()))
        .max_by_key(|s| s.creation_epoch_secs);

    if let Some(snap) = newest {
        zfs::set_property(&snap.full_name, origin::ORIGIN_PROPERTY, &claimed.format())?;
    }

    Ok(())
}
