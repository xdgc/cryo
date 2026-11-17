// vim: ts=4 et

//! Renders the same human-readable text both for a standalone `cryo
//! status`/`cryo run` invocation (printed directly to stdout) and for
//! a daemon answering those same requests over the local control
//! socket (sent back as the response body) -- see ipc.rs.

use std::fmt::Write;

use crate::discover::Model;
use crate::policy;
use crate::snapshot::ActionResult;
use crate::span::Span;
use crate::target::{Path as TargetPath, Target, Transport};
use crate::timestamp::Timestamp;

pub fn render_status(model: &Model, now: Timestamp) -> String {
    let mut out = String::new();

    let mut pools: Vec<&String> = model.pool_reserve.keys().collect();
    pools.sort();

    for pool in pools {
        let _ = writeln!(out, "{pool} reserve {}", format_reserve(model.pool_reserve[pool]));
    }

    let mut names: Vec<&String> = model.datasets.keys().collect();
    names.sort();

    for name in names {
        let state = &model.datasets[name];

        if state.policies.is_empty()
            && state.snapshots.is_empty()
            && state.limit.is_none()
            && state.targets.is_empty()
        {
            continue;
        }

        let _ = writeln!(out, "{name}");

        if let Some(limit) = state.limit {
            let _ = writeln!(
                out,
                "  limit {} (used {})",
                format_bytes(limit),
                format_bytes(state.used_with_snapshots())
            );
        }

        for target in &state.targets {
            let _ = writeln!(out, "  target {:<12} {}", target_label(target), describe_target(target));
        }

        let mut policies: Vec<&policy::Policy> = state.policies.iter().collect();
        policies.sort_by(|a, b| a.name.cmp(&b.name));

        for p in policies {
            let label = policy_label(&p.name);
            let due = describe_due(state.last_fired.get(&p.name), p.frequency, now);
            let source = state
                .policy_sources
                .get(&p.name)
                .map(String::as_str)
                .unwrap_or("local");
            let _ = writeln!(
                out,
                "  policy {label:<12} {}/{}  {due}  [{source}]",
                p.frequency.format(),
                policy::format_retention(p.retention)
            );
        }

        let mut snaps: Vec<_> = state.snapshots.iter().collect();
        snaps.sort_by_key(|s| s.taken_at);

        for snap in snaps {
            let claims = if snap.claimed_by.is_empty() {
                "(none)".to_string()
            } else {
                snap.claimed_by
                    .iter()
                    .map(|n| policy_label(n))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let marker = if snap.instant { " instant" } else { "" };
            let _ = writeln!(out, "  snapshot {}{marker}  claimed by: {claims}", snap.full_name);
        }
    }

    out
}

pub fn render_action_result(result: &ActionResult) -> String {
    let mut out = String::new();

    for full_name in &result.taken {
        let _ = writeln!(out, "took {full_name}");
    }

    for full_name in &result.destroyed {
        let _ = writeln!(out, "destroyed {full_name}");
    }

    for message in &result.replicated {
        let _ = writeln!(out, "{message}");
    }

    out
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("T", 1024u64.pow(4)),
        ("G", 1024 * 1024 * 1024),
        ("M", 1024 * 1024),
        ("K", 1024),
    ];

    for (suffix, unit) in UNITS {
        if bytes >= unit {
            return format!("{:.1}{suffix}", bytes as f64 / unit as f64);
        }
    }

    format!("{bytes}B")
}

fn format_reserve(reserve: policy::Reserve) -> String {
    match reserve {
        policy::Reserve::Fixed(floor) => format_bytes(floor),
        policy::Reserve::HighLow { high, low } => format!("{}:{}", format_bytes(high), format_bytes(low)),
    }
}

fn target_label(target: &Target) -> &str {
    if target.name.is_empty() { "(default)" } else { &target.name }
}

fn describe_target(target: &Target) -> String {
    let path = match &target.path {
        None => "(same as origin)".to_string(),
        Some(TargetPath::Zfs(p)) => p.clone(),
        Some(TargetPath::Vfs(p)) => p.clone(),
    };

    let transport = match &target.transport {
        Transport::Local => "local".to_string(),
        Transport::Ssh => "ssh".to_string(),
        Transport::Pipe(detail) => format!("pipe:{detail}"),
    };

    match &target.endpoint {
        Some(endpoint) => format!("{endpoint}:{path} [{transport}]"),
        None => format!("{path} [{transport}]"),
    }
}

fn policy_label(name: &str) -> &str {
    if name.is_empty() { "(default)" } else { name }
}

fn describe_due(last_fired: Option<&Timestamp>, frequency: Span, now: Timestamp) -> String {
    match last_fired {
        None => "due now (never fired)".to_string(),
        Some(last) => {
            let elapsed = last.elapsed_secs_until(now);
            let remaining = frequency.minutes() as i64 * 60 - elapsed;
            if remaining <= 0 {
                format!("last fired {}, due now", last.format())
            } else {
                let remaining_minutes = ((remaining + 59) / 60) as u64;
                format!(
                    "last fired {}, due in {}",
                    last.format(),
                    Span::from_minutes(remaining_minutes).format()
                )
            }
        }
    }
}
