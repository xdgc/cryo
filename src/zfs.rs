// vim: ts=4 et

//! A thin wrapper around the `zfs`/`zpool` binaries, using their
//! machine-readable output modes (`-H -p`) rather than linking against
//! `libzfs_core` -- see cryo.md, "Implementation notes", for why.

use std::process::{Child, Command, Stdio};

use crate::error::{CryoError, Result};

/// One row of `zfs get`/`zfs list` output.
#[derive(Debug, Clone)]
pub struct Property {
    pub dataset: String,
    pub name: String,
    pub value: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct SnapshotListing {
    pub full_name: String, // "dataset@snapshot"
    pub creation_epoch_secs: i64,
}

fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(CryoError::Command(format!(
            "{program} {}: {stderr}",
            args.join(" ")
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn list_pools() -> Result<Vec<String>> {
    let out = run("zpool", &["list", "-H", "-o", "name"])?;
    Ok(out.lines().map(str::to_string).collect())
}

pub fn pool_guid(pool: &str) -> Result<u64> {
    let out = run("zpool", &["get", "-H", "-p", "-o", "value", "guid", pool])?;
    out.trim()
        .parse()
        .map_err(|_| CryoError::Command(format!("unexpected 'zpool get guid' output for {pool}: {out:?}")))
}

pub fn dataset_guid(dataset: &str) -> Result<u64> {
    let out = run("zfs", &["get", "-H", "-p", "-o", "value", "guid", dataset])?;
    out.trim()
        .parse()
        .map_err(|_| CryoError::Command(format!("unexpected 'zfs get guid' output for {dataset}: {out:?}")))
}

pub fn pool_free(pool: &str) -> Result<u64> {
    let out = run("zpool", &["list", "-H", "-p", "-o", "free", pool])?;
    out.trim()
        .parse()
        .map_err(|_| CryoError::Command(format!("unexpected 'zpool list' output for {pool}: {out:?}")))
}

/// `referenced` (current data) plus `usedbysnapshots` (space held
/// uniquely by this dataset's own snapshots) -- re-fetched fresh for
/// a single dataset after a destroy, since ZFS's copy-on-write block
/// sharing between snapshots means space freed can't be estimated
/// from the snapshot alone; only zfs's own accounting is authoritative.
pub fn dataset_usage(dataset: &str) -> Result<(u64, u64)> {
    let out = run(
        "zfs",
        &["get", "-H", "-p", "-o", "property,value", "referenced,usedbysnapshots", dataset],
    )?;

    let mut referenced = 0u64;
    let mut used_by_snapshots = 0u64;

    for line in out.lines() {
        if let Some((property, value)) = line.split_once('\t') {
            match property {
                "referenced" => referenced = value.parse().unwrap_or(0),
                "usedbysnapshots" => used_by_snapshots = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }

    Ok((referenced, used_by_snapshots))
}

/// Every property on every filesystem/volume under `root`, inheritance
/// already resolved by zfs itself. Callers filter for the `cryo:*`
/// ones they care about -- there's no way to ask `zfs get` for "every
/// property with this prefix" since policy/claim property names are
/// open-ended (`cryo:snapshot-<name>`), so we fetch everything once per
/// root and filter client-side.
pub fn all_properties(root: &str) -> Result<Vec<Property>> {
    let out = run(
        "zfs",
        &[
            "get", "-H", "-p", "-r", "-t", "filesystem,volume", "-o",
            "name,property,value,source", "all", root,
        ],
    )?;
    Ok(parse_property_rows(&out))
}

fn parse_property_rows(out: &str) -> Vec<Property> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(Property {
                dataset: fields.next()?.to_string(),
                name: fields.next()?.to_string(),
                value: fields.next()?.to_string(),
                source: fields.next()?.to_string(),
            })
        })
        .collect()
}

/// Every snapshot recursively under `root`, with its creation time.
pub fn list_snapshots(root: &str) -> Result<Vec<SnapshotListing>> {
    let out = run(
        "zfs",
        &[
            "list", "-H", "-p", "-r", "-t", "snapshot", "-o", "name,creation", root,
        ],
    )?;

    out.lines()
        .filter_map(|line| {
            let (name, creation) = line.split_once('\t')?;
            let creation_epoch_secs = creation.parse().ok()?;
            Some(SnapshotListing {
                full_name: name.to_string(),
                creation_epoch_secs,
            })
        })
        .map(Ok)
        .collect()
}

/// `cryo:claimed-by` for each of `snapshots`, batched into a single
/// `zfs get` call. Snapshots with no such property set (the common
/// case: nothing to claim) are simply absent from the result.
pub fn claimed_by_batch(snapshots: &[String]) -> Result<Vec<(String, String)>> {
    if snapshots.is_empty() {
        return Ok(Vec::new());
    }

    let mut args = vec!["get", "-H", "-p", "-o", "name,value", "cryo:claimed-by"];
    args.extend(snapshots.iter().map(String::as_str));
    let out = run("zfs", &args)?;

    Ok(out
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('\t')?;
            if value == "-" {
                None
            } else {
                Some((name.to_string(), value.to_string()))
            }
        })
        .collect())
}

pub fn get_property(dataset: &str, property: &str) -> Result<String> {
    let out = run("zfs", &["get", "-H", "-p", "-o", "value", property, dataset])?;
    Ok(out.trim().to_string())
}

pub fn dataset_exists(dataset: &str) -> Result<bool> {
    let status = Command::new("zfs")
        .args(["list", "-H", "-o", "name", dataset])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

pub fn is_encrypted(dataset: &str) -> Result<bool> {
    let out = run("zfs", &["get", "-H", "-p", "-o", "value", "encryption", dataset])?;
    Ok(out.trim() != "off")
}

/// `zfs send [-w] [-i baseline] snapshot`, stdout piped so the caller
/// can stream it out as `DATA` frames incrementally rather than
/// buffering the whole thing.
pub fn spawn_send(snapshot: &str, baseline: Option<&str>, raw: bool) -> Result<Child> {
    let mut args: Vec<&str> = vec!["send"];

    if raw {
        args.push("-w");
    }

    if let Some(baseline) = baseline {
        args.push("-i");
        args.push(baseline);
    }

    args.push(snapshot);

    Ok(Command::new("zfs").args(&args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?)
}

/// `zfs send -t token`, continuing a previously interrupted send from
/// wherever it left off -- cryo.md (k). The token already encodes
/// which dataset and snapshot this resumes; the local snapshot it
/// refers to just has to still exist.
pub fn spawn_send_resume(token: &str) -> Result<Child> {
    Ok(Command::new("zfs")
        .args(["send", "-t", token])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}

/// `zfs receive -s -F dataset`, stdin piped so the caller can feed it
/// incoming `DATA` frame payloads as they arrive. `-F` rolls the
/// destination back to match the incoming stream if it's diverged --
/// reasonable for a dataset that's entirely cryo's to manage on the
/// replica side. `-s` saves partial state if the stream is
/// interrupted, so a later attempt can resume it (see
/// `resume_token`/`spawn_send_resume`) instead of starting over.
pub fn spawn_receive(dataset: &str) -> Result<Child> {
    Ok(Command::new("zfs")
        .args(["receive", "-s", "-F", dataset])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}

/// The destination's pending resume token, if an earlier transfer to
/// it was interrupted after `-s` had something to save. `None` if
/// there's nothing pending.
pub fn resume_token(dataset: &str) -> Result<Option<String>> {
    let value = get_property(dataset, "receive_resume_token")?;
    Ok(if value == "-" { None } else { Some(value) })
}

pub fn create_snapshot(full_name: &str) -> Result<()> {
    run("zfs", &["snapshot", full_name]).map(|_| ())
}

pub fn destroy_snapshot(full_name: &str) -> Result<()> {
    run("zfs", &["destroy", full_name]).map(|_| ())
}

pub fn set_property(target: &str, property: &str, value: &str) -> Result<()> {
    run("zfs", &["set", &format!("{property}={value}"), target]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_property_rows() {
        let rows = parse_property_rows("pool/a\tcryo:snapshot\t15m/8\tlocal\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].dataset, "pool/a");
        assert_eq!(rows[0].name, "cryo:snapshot");
        assert_eq!(rows[0].value, "15m/8");
        assert_eq!(rows[0].source, "local");
    }
}
