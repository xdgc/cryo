// vim: ts=4 et

//! `cryo:origin = <hostname>|<dataset-path>|<pool-guid>|<dataset-guid>`
//! -- cryo.md, "Replication behaviors" (g). Set on a replication
//! target dataset (and its snapshots) by the replication procedure,
//! identifying where a non-local-origin dataset actually came from.

use crate::error::{CryoError, Result};

pub const ORIGIN_PROPERTY: &str = "cryo:origin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub hostname: String,
    pub dataset_path: String,
    pub pool_guid: u64,
    pub dataset_guid: u64,
}

impl Origin {
    pub fn format(&self) -> String {
        format!("{}|{}|{}|{}", self.hostname, self.dataset_path, self.pool_guid, self.dataset_guid)
    }

    pub fn parse(value: &str) -> Result<Origin> {
        let parts: Vec<&str> = value.split('|').collect();
        let [hostname, dataset_path, pool_guid, dataset_guid] = parts.as_slice() else {
            return Err(bad_origin(value));
        };

        Ok(Origin {
            hostname: hostname.to_string(),
            dataset_path: dataset_path.to_string(),
            pool_guid: pool_guid.parse().map_err(|_| bad_origin(value))?,
            dataset_guid: dataset_guid.parse().map_err(|_| bad_origin(value))?,
        })
    }

    /// Whether `self` (what's already recorded) and `claimed` (what a
    /// replication session's hello just asserted) describe the same
    /// lineage. A hostname-only match isn't enough -- see cryo.md (g)
    /// on why the GUIDs are what actually matter.
    pub fn matches(&self, claimed: &Origin) -> bool {
        self.hostname == claimed.hostname
            && self.pool_guid == claimed.pool_guid
            && self.dataset_guid == claimed.dataset_guid
    }
}

fn bad_origin(value: &str) -> CryoError {
    CryoError::Parse(format!("'{value}' is not a valid cryo:origin value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_format_and_parse() {
        let origin = Origin {
            hostname: "bagofholding".to_string(),
            dataset_path: "pool1/archive".to_string(),
            pool_guid: 123,
            dataset_guid: 456,
        };
        assert_eq!(Origin::parse(&origin.format()).unwrap(), origin);
    }

    #[test]
    fn matches_on_guids_not_just_hostname() {
        let recorded = Origin::parse("host|pool1/a|1|2").unwrap();
        let same = Origin::parse("host|pool1/a|1|2").unwrap();
        let rebuilt = Origin::parse("host|pool1/a|1|999").unwrap();

        assert!(recorded.matches(&same));
        assert!(!recorded.matches(&rebuilt));
    }

    #[test]
    fn rejects_malformed_values() {
        assert!(Origin::parse("host|path|1").is_err());
        assert!(Origin::parse("host|path|notanumber|2").is_err());
    }
}
