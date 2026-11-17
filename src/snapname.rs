// vim: ts=4 et

//! Cryo-managed snapshot naming: `dataset@cryo-YYYYMMDD.HHMMSS`, or
//! `dataset@cryo-instant-YYYYMMDD.HHMMSS` for a manual snapshot that
//! never auto-expires. The name carries no policy identity -- see
//! cryo.md, "Snapshot behaviors" (f).

use crate::timestamp::Timestamp;

pub const TAG: &str = "cryo";

pub fn scheduled_name(dataset: &str, taken_at: Timestamp) -> String {
    format!("{dataset}@{TAG}-{}", taken_at.format())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSnapshot {
    pub dataset: String,
    pub instant: bool,
    pub taken_at: Timestamp,
}

/// Recognizes a `dataset@snapshot` full name as cryo-managed. Returns
/// `None` for anything else -- including snapshots made by other
/// tools (e.g. a leftover `zest-...` snapshot) -- rather than an
/// error, since most snapshots on a real pool won't be cryo's.
pub fn parse(full_name: &str) -> Option<ManagedSnapshot> {
    let (dataset, snap_name) = full_name.split_once('@')?;
    let rest = snap_name.strip_prefix(TAG)?.strip_prefix('-')?;

    let (instant, timestamp_text) = match rest.strip_prefix("instant-") {
        Some(rest) => (true, rest),
        None => (false, rest),
    };

    let taken_at = Timestamp::parse(timestamp_text).ok()?;

    Some(ManagedSnapshot {
        dataset: dataset.to_string(),
        instant,
        taken_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_scheduled_name() {
        let ts = Timestamp::from_epoch_secs(1_700_000_000);
        let name = scheduled_name("pool/a", ts);
        let parsed = parse(&name).unwrap();
        assert_eq!(parsed.dataset, "pool/a");
        assert!(!parsed.instant);
        assert_eq!(parsed.taken_at, ts);
    }

    #[test]
    fn recognizes_instant_name() {
        let ts = Timestamp::from_epoch_secs(1_700_000_000);
        let name = format!("pool/a@{TAG}-instant-{}", ts.format());
        let parsed = parse(&name).unwrap();
        assert!(parsed.instant);
        assert_eq!(parsed.taken_at, ts);
    }

    #[test]
    fn ignores_foreign_snapshots() {
        assert!(parse("pool/a@zest-20161127.0300-1h").is_none());
        assert!(parse("pool/a@manual-backup").is_none());
        assert!(parse("pool/a").is_none());
    }
}
