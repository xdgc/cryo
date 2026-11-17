// vim: ts=4 et

//! `cryo:target[-<name>]`: `[endpoint:]path [transport:detail]`. See
//! cryo.md, "Replication behaviors" (a)-(e).

use crate::error::{CryoError, Result};

pub const TARGET_PROPERTY: &str = "cryo:target";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// `ssh <endpoint> cryo server`, tuned via `~/.ssh/config`.
    Ssh,
    /// Run `detail` as a command that connects to `cryo server`.
    Pipe(String),
    /// Run `cryo server` locally.
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Path {
    /// Begins with `.` or `/`: a filesystem location, snapshot
    /// streams are files on disk.
    Vfs(String),
    /// Begins with a letter: a zfs path, pool optionally followed by
    /// a parent dataset.
    Zfs(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub endpoint: Option<String>,
    pub path: Option<Path>,
    pub transport: Transport,
}

/// The `cryo:target`/`cryo:target-<name>` counterpart of
/// `policy::policy_name_from_property`.
pub fn target_name_from_property(property: &str) -> Option<&str> {
    if property == TARGET_PROPERTY {
        return Some("");
    }

    property
        .strip_prefix(TARGET_PROPERTY)
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|name| !name.is_empty())
}

pub fn parse_target_value(name: &str, value: &str) -> Result<Target> {
    let value = value.trim();
    let (location, transport_spec) = match value.split_once(char::is_whitespace) {
        Some((location, rest)) => (location, Some(rest.trim())),
        None => (value, None),
    };

    let (endpoint, path_text) = split_endpoint(location);

    let path = if path_text.is_empty() {
        None
    } else if path_text.starts_with('.') || path_text.starts_with('/') {
        Some(Path::Vfs(path_text.to_string()))
    } else if path_text.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        Some(Path::Zfs(path_text.to_string()))
    } else {
        return Err(bad_target(name, value));
    };

    let transport = match transport_spec {
        None => {
            if endpoint.is_some() {
                Transport::Ssh
            } else {
                Transport::Local
            }
        }
        Some(spec) => match spec.split_once(':') {
            Some(("ssh", "")) => Transport::Ssh,
            Some(("pipe", detail)) if !detail.is_empty() => Transport::Pipe(detail.to_string()),
            Some(("local", "")) => Transport::Local,
            _ => return Err(bad_target(name, value)),
        },
    };

    if endpoint.is_none() && path.is_none() {
        return Err(bad_target(name, value));
    }

    Ok(Target {
        name: name.to_string(),
        endpoint,
        path,
        transport,
    })
}

/// Splits `location` into an optional endpoint and the remaining path
/// text. A leading `.` or `/` is unambiguously a path (per cryo.md,
/// and incidentally never a valid ssh endpoint either), so only a
/// colon in anything else is considered an endpoint separator. Zfs
/// pool names can't themselves contain a colon, so a colon appearing
/// before any `/` can only be an endpoint separator, never part of a
/// zfs path -- this heuristic has no real ambiguity to resolve.
fn split_endpoint(location: &str) -> (Option<String>, &str) {
    if location.starts_with('.') || location.starts_with('/') {
        return (None, location);
    }

    match location.split_once(':') {
        Some((endpoint, path)) if !endpoint.contains('/') => (Some(endpoint.to_string()), path),
        _ => (None, location),
    }
}

fn bad_target(name: &str, value: &str) -> CryoError {
    CryoError::Parse(format!("target '{name}': '{value}' is not a valid target"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_target_name_from_property() {
        assert_eq!(target_name_from_property("cryo:target"), Some(""));
        assert_eq!(target_name_from_property("cryo:target-cloud"), Some("cloud"));
        assert_eq!(target_name_from_property("cryo:snapshot"), None);
    }

    #[test]
    fn parses_bare_zfs_path_no_endpoint() {
        let t = parse_target_value("", "backup/pool1").unwrap();
        assert_eq!(t.endpoint, None);
        assert_eq!(t.path, Some(Path::Zfs("backup/pool1".to_string())));
        assert_eq!(t.transport, Transport::Local);
    }

    #[test]
    fn parses_endpoint_and_zfs_path_defaults_to_ssh() {
        let t = parse_target_value("", "backuphost:pool1/archive").unwrap();
        assert_eq!(t.endpoint, Some("backuphost".to_string()));
        assert_eq!(t.path, Some(Path::Zfs("pool1/archive".to_string())));
        assert_eq!(t.transport, Transport::Ssh);
    }

    #[test]
    fn parses_endpoint_with_no_path() {
        let t = parse_target_value("", "backuphost:").unwrap();
        assert_eq!(t.endpoint, Some("backuphost".to_string()));
        assert_eq!(t.path, None);
        assert_eq!(t.transport, Transport::Ssh);
    }

    #[test]
    fn parses_vfs_path() {
        let t = parse_target_value("", "/mnt/backups/pool1").unwrap();
        assert_eq!(t.endpoint, None);
        assert_eq!(t.path, Some(Path::Vfs("/mnt/backups/pool1".to_string())));
        assert_eq!(t.transport, Transport::Local);

        let t = parse_target_value("", "./relative/backups").unwrap();
        assert_eq!(t.path, Some(Path::Vfs("./relative/backups".to_string())));
    }

    #[test]
    fn parses_explicit_pipe_transport() {
        let t = parse_target_value("", "pool1/archive pipe:nc backuphost 9999").unwrap();
        assert_eq!(t.path, Some(Path::Zfs("pool1/archive".to_string())));
        assert_eq!(t.transport, Transport::Pipe("nc backuphost 9999".to_string()));
    }

    #[test]
    fn parses_explicit_local_transport_override() {
        let t = parse_target_value("", "backuphost:pool1 local:").unwrap();
        assert_eq!(t.endpoint, Some("backuphost".to_string()));
        assert_eq!(t.transport, Transport::Local);
    }

    #[test]
    fn rejects_malformed_targets() {
        for bad in ["", "9bad/path", "host pipe:"] {
            assert!(parse_target_value("", bad).is_err(), "expected '{bad}' to be rejected");
        }
    }
}
