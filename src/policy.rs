// vim: ts=4 et

//! `cryo:snapshot[-<name>]` policies: a frequency and a retention,
//! e.g. "15m/2h" (every 15 minutes, keep 2 hours' worth) or
//! "1d/indefinite". See cryo.md, "Snapshot behaviors".

use crate::error::{CryoError, Result};
use crate::size;
use crate::span::Span;

pub const SNAPSHOT_PROPERTY: &str = "cryo:snapshot";
pub const LAST_PROPERTY: &str = "cryo:last";
pub const CLAIMED_BY_PROPERTY: &str = "cryo:claimed-by";
pub const LIMIT_PROPERTY: &str = "cryo:limit";
pub const RESERVE_PROPERTY: &str = "cryo:reserve";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    Count(u64),
    Duration(Span),
    Indefinite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// The suffix after `cryo:snapshot-`, or empty for the bare
    /// `cryo:snapshot` policy. Also how this policy names itself in a
    /// snapshot's `cryo:claimed-by` list, and in `cryo:last[-name]`.
    pub name: String,
    pub frequency: Span,
    pub retention: Retention,
}

/// Returns `Some(policy name)` if `property` is `cryo:snapshot` or
/// `cryo:snapshot-<name>`; `None` for anything else (including the
/// unrelated `cryo:snapshot` *prefix* collision `cryo:snapshotxyz`,
/// which is simply not a policy property).
pub fn policy_name_from_property(property: &str) -> Option<&str> {
    if property == SNAPSHOT_PROPERTY {
        return Some("");
    }

    property
        .strip_prefix(SNAPSHOT_PROPERTY)
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|name| is_valid_policy_name(name))
}

/// The `cryo:last`/`cryo:last-<name>` counterpart of
/// `policy_name_from_property`.
pub fn last_policy_name_from_property(property: &str) -> Option<&str> {
    if property == LAST_PROPERTY {
        return Some("");
    }

    property
        .strip_prefix(LAST_PROPERTY)
        .and_then(|rest| rest.strip_prefix('-'))
        .filter(|name| is_valid_policy_name(name))
}

fn is_valid_policy_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn parse_policy_value(name: &str, value: &str) -> Result<Policy> {
    let (freq_text, retention_text) = value.split_once('/').ok_or_else(|| {
        CryoError::Parse(format!(
            "policy '{name}': '{value}' is not <frequency>/<retention>"
        ))
    })?;

    let frequency = Span::parse(freq_text)?;
    let retention = parse_retention(retention_text)?;

    Ok(Policy {
        name: name.to_string(),
        frequency,
        retention,
    })
}

fn parse_retention(text: &str) -> Result<Retention> {
    if text == "indefinite" || text == "*" {
        return Ok(Retention::Indefinite);
    }

    if text.chars().all(|c| c.is_ascii_digit()) && !text.is_empty() {
        let count: u64 = text
            .parse()
            .map_err(|_| CryoError::Parse(format!("'{text}' is not a valid retention count")))?;
        return Ok(Retention::Count(count));
    }

    Ok(Retention::Duration(Span::parse(text)?))
}

pub fn format_retention(retention: Retention) -> String {
    match retention {
        Retention::Count(n) => n.to_string(),
        Retention::Duration(span) => span.format(),
        Retention::Indefinite => "indefinite".to_string(),
    }
}

/// Property name for this policy's scheduling state: `cryo:last` for
/// the bare policy, `cryo:last-<name>` for a named one.
pub fn last_property_for(policy_name: &str) -> String {
    if policy_name.is_empty() {
        LAST_PROPERTY.to_string()
    } else {
        format!("{LAST_PROPERTY}-{policy_name}")
    }
}

pub fn parse_limit_value(value: &str) -> Result<u64> {
    size::parse(value)
}

/// `cryo:reserve`: a minimum free-space floor on the pool, or a
/// `high:low` pair for hysteresis -- trigger cleanup once free space
/// falls under `low`, and keep going until it's back above `high`.
/// `high` must exceed `low`, or the pair can never do anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reserve {
    Fixed(u64),
    HighLow { high: u64, low: u64 },
}

pub fn parse_reserve_value(value: &str) -> Result<Reserve> {
    match value.split_once(':') {
        None => Ok(Reserve::Fixed(size::parse(value)?)),
        Some((high_text, low_text)) => {
            let high = size::parse(high_text)?;
            let low = size::parse(low_text)?;

            if high <= low {
                return Err(CryoError::Parse(format!(
                    "'{value}' is not a valid high:low reserve pair (high must exceed low)"
                )));
            }

            Ok(Reserve::HighLow { high, low })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_policy_name_from_property() {
        assert_eq!(policy_name_from_property("cryo:snapshot"), Some(""));
        assert_eq!(
            policy_name_from_property("cryo:snapshot-monthly"),
            Some("monthly")
        );
        assert_eq!(policy_name_from_property("cryo:snapshot-"), None);
        assert_eq!(policy_name_from_property("cryo:last"), None);
        assert_eq!(policy_name_from_property("cryo:snapshotxyz"), None);
    }

    #[test]
    fn parses_frequency_and_count_retention() {
        let p = parse_policy_value("", "15m/8").unwrap();
        assert_eq!(p.frequency.minutes(), 15);
        assert_eq!(p.retention, Retention::Count(8));
    }

    #[test]
    fn parses_frequency_and_duration_retention() {
        let p = parse_policy_value("monthly", "1h/8h").unwrap();
        assert_eq!(p.frequency.minutes(), 60);
        assert_eq!(p.retention, Retention::Duration(Span::from_minutes(480)));
    }

    #[test]
    fn parses_indefinite_retention() {
        for text in ["1d/indefinite", "1d/*"] {
            let p = parse_policy_value("archive", text).unwrap();
            assert_eq!(p.retention, Retention::Indefinite);
        }
    }

    #[test]
    fn rejects_malformed_values() {
        assert!(parse_policy_value("", "15m").is_err());
        assert!(parse_policy_value("", "nope/8h").is_err());
    }

    #[test]
    fn extracts_policy_name_from_last_property() {
        assert_eq!(last_policy_name_from_property("cryo:last"), Some(""));
        assert_eq!(
            last_policy_name_from_property("cryo:last-monthly"),
            Some("monthly")
        );
        assert_eq!(last_policy_name_from_property("cryo:snapshot"), None);
    }

    #[test]
    fn builds_last_property_name() {
        assert_eq!(last_property_for(""), "cryo:last");
        assert_eq!(last_property_for("monthly"), "cryo:last-monthly");
    }

    #[test]
    fn parses_limit() {
        assert_eq!(parse_limit_value("10G").unwrap(), 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn parses_fixed_reserve() {
        assert_eq!(parse_reserve_value("5g").unwrap(), Reserve::Fixed(5_000_000_000));
    }

    #[test]
    fn parses_high_low_reserve() {
        assert_eq!(
            parse_reserve_value("20g:10g").unwrap(),
            Reserve::HighLow { high: 20_000_000_000, low: 10_000_000_000 }
        );
    }

    #[test]
    fn rejects_inverted_high_low_reserve() {
        assert!(parse_reserve_value("10g:20g").is_err());
        assert!(parse_reserve_value("10g:10g").is_err());
    }
}
