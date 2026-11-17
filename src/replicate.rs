// vim: ts=4 et

//! Client side of replication: pick what needs sending, and either
//! open a wire-protocol session and negotiate a baseline against
//! what the target already has (`one_shot_wire`), or write the
//! stream straight to a file for a VFS target (`one_shot_file`) --
//! see cryo.md, "Replication behaviors" (d): "A path that begins with
//! `.` or `/` is a VFS filesystem location and snapshot streams are
//! files on disk." There's no peer to negotiate with there, so the
//! existing-baseline question is answered by looking at what stream
//! files are already sitting in that directory instead of asking a
//! `cryo server`.

use std::fs;
use std::io;
use std::path::{Path as FsPath, PathBuf};

use crate::error::{CryoError, Result};
use crate::handshake;
use crate::protocol::{Frame, FrameType};
use crate::snapname;
use crate::stream;
use crate::target::{Path as TargetPath, Target};
use crate::timestamp::Timestamp;
use crate::transport;
use crate::zfs;

pub struct Outcome {
    pub message: String,
}

pub fn one_shot(dataset: &str, target: &Target) -> Result<Outcome> {
    match &target.path {
        Some(TargetPath::Vfs(base_dir)) => one_shot_file(dataset, base_dir),
        _ => one_shot_wire(dataset, target),
    }
}

fn one_shot_wire(dataset: &str, target: &Target) -> Result<Outcome> {
    let destination_path = resolve_zfs_destination(dataset, target)?;

    let local = cryo_snapshot_names(dataset)?;
    let (_, latest_name) = local
        .first()
        .cloned()
        .ok_or_else(|| CryoError::Command(format!("{dataset} has no cryo-managed snapshots to replicate")))?;
    let latest_full = format!("{dataset}@{latest_name}");

    let mut session = transport::connect(target)?;
    let ack = handshake::send_hello(&mut session, dataset, &destination_path)?;

    // A pending resume always takes priority: zfs won't accept any
    // other receive onto this destination until it's either resumed
    // or explicitly aborted, so "up to date" doesn't apply while one
    // is outstanding.
    let resuming = ack.resume_token.is_some();
    let up_to_date = !resuming && ack.existing_snapshots.iter().any(|name| name == &latest_name);
    handshake::send_proceed(&mut session, resuming || !up_to_date)?;

    if up_to_date {
        session.finish()?;
        return Ok(Outcome { message: format!("{destination_path} already has {latest_name}") });
    }

    let mut send = match &ack.resume_token {
        Some(token) => zfs::spawn_send_resume(token)?,
        None => {
            let baseline_full = local
                .iter()
                .find(|(_, name)| ack.existing_snapshots.contains(name))
                .map(|(_, name)| format!("{dataset}@{name}"));
            let encrypted = zfs::is_encrypted(dataset)?;
            zfs::spawn_send(&latest_full, baseline_full.as_deref(), encrypted)?
        }
    };
    let send_stdout = send.stdout.take().expect("spawned with piped stdout");

    let send_result = stream::send_stream(send_stdout, &mut session.writer, stream::HEARTBEAT_INTERVAL);
    let send_status = send.wait()?;
    let send_ok = send_result.is_ok() && send_status.success();

    let terminator = if send_ok { FrameType::EndOfStream } else { FrameType::Abort };
    Frame::new(terminator, Vec::new()).write_to(&mut session.writer)?;

    if !send_ok {
        let _ = session.finish();
        return Err(CryoError::Command(match send_result {
            Err(e) => e.to_string(),
            Ok(_) => format!("zfs send exited with {send_status}"),
        }));
    }

    handshake::check_result(&mut session)?;
    session.finish()?;

    let message = if resuming {
        format!("resumed interrupted transfer to {destination_path}")
    } else {
        format!("replicated {latest_name} to {destination_path}")
    };
    Ok(Outcome { message })
}

/// Writes a VFS target's stream straight to `<base_dir>/<dataset
/// path>/<snapshot name>.zfs`, mirroring the dataset's own path
/// structure as nested directories. Written first to a `.partial`
/// sibling and renamed into place only once `zfs send` has actually
/// finished successfully, so a crash or kill mid-write never leaves a
/// truncated file sitting under a name that later looks like a valid
/// baseline.
fn one_shot_file(dataset: &str, base_dir: &str) -> Result<Outcome> {
    let dir = FsPath::new(base_dir).join(dataset);
    fs::create_dir_all(&dir)?;

    let local = cryo_snapshot_names(dataset)?;
    let (_, latest_name) = local
        .first()
        .cloned()
        .ok_or_else(|| CryoError::Command(format!("{dataset} has no cryo-managed snapshots to replicate")))?;
    let latest_full = format!("{dataset}@{latest_name}");

    let existing = existing_stream_names(&dir)?;

    if existing.contains(&latest_name) {
        return Ok(Outcome { message: format!("{} already has {latest_name}", dir.display()) });
    }

    let baseline_full = local
        .iter()
        .find(|(_, name)| existing.contains(name))
        .map(|(_, name)| format!("{dataset}@{name}"));

    let encrypted = zfs::is_encrypted(dataset)?;
    let mut send = zfs::spawn_send(&latest_full, baseline_full.as_deref(), encrypted)?;
    let mut send_stdout = send.stdout.take().expect("spawned with piped stdout");

    let final_path: PathBuf = dir.join(format!("{latest_name}.zfs"));
    let tmp_path: PathBuf = dir.join(format!("{latest_name}.zfs.partial"));

    let copy_result = {
        let mut file = fs::File::create(&tmp_path)?;
        io::copy(&mut send_stdout, &mut file).map_err(CryoError::from)
    };
    let send_status = send.wait()?;
    let ok = copy_result.is_ok() && send_status.success();

    if !ok {
        let _ = fs::remove_file(&tmp_path);
        return Err(CryoError::Command(match copy_result {
            Err(e) => e.to_string(),
            Ok(_) => format!("zfs send exited with {send_status}"),
        }));
    }

    fs::rename(&tmp_path, &final_path)?;

    Ok(Outcome { message: format!("wrote {} to {}", latest_name, final_path.display()) })
}

/// cryo.md (d): "If path is absent, it's the same as the origin."
fn resolve_zfs_destination(dataset: &str, target: &Target) -> Result<String> {
    match &target.path {
        None => Ok(dataset.to_string()),
        Some(TargetPath::Zfs(path)) => Ok(path.clone()),
        Some(TargetPath::Vfs(_)) => unreachable!("VFS targets are routed to one_shot_file before this point"),
    }
}

/// Bare snapshot names (`<name>` without the `.zfs` suffix) of
/// already-written, complete stream files in `dir`. A lone
/// `.partial` file left behind by an interrupted write is correctly
/// invisible here -- it doesn't have the plain `.zfs` suffix this
/// looks for.
fn existing_stream_names(dir: &FsPath) -> Result<Vec<String>> {
    let mut names = Vec::new();

    if !dir.exists() {
        return Ok(names);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str()
            && let Some(snap_name) = name.strip_suffix(".zfs")
        {
            names.push(snap_name.to_string());
        }
    }

    Ok(names)
}

/// `dataset`'s own cryo-managed snapshots (not descendants'), as
/// (creation time, bare snapshot name) pairs, newest first.
fn cryo_snapshot_names(dataset: &str) -> Result<Vec<(Timestamp, String)>> {
    let mut names: Vec<(Timestamp, String)> = zfs::list_snapshots(dataset)?
        .into_iter()
        .filter(|s| s.full_name.split_once('@').map(|(ds, _)| ds) == Some(dataset))
        .filter(|s| snapname::parse(&s.full_name).is_some())
        .filter_map(|s| {
            let (_, snap_name) = s.full_name.split_once('@')?;
            Some((Timestamp::from_epoch_secs(s.creation_epoch_secs), snap_name.to_string()))
        })
        .collect();

    names.sort_by_key(|(taken_at, _)| *taken_at);
    names.reverse();
    Ok(names)
}
