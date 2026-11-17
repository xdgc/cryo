// vim: ts=4 et

//! UTC timestamps for snapshot names and `cryo:last*` property values,
//! both formatted as `YYYYMMDD.HHMMSS`. Calendar conversion is done by
//! hand (Howard Hinnant's days_from_civil/civil_from_days algorithm)
//! rather than pulling in a date/time crate, since it's a small, well-
//! known, dependency-free piece of arithmetic and this program has no
//! other use for a full calendar library.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CryoError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(i64); // seconds since the Unix epoch, UTC

impl Timestamp {
    pub fn now() -> Timestamp {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before 1970")
            .as_secs();
        Timestamp(secs as i64)
    }

    pub fn from_epoch_secs(secs: i64) -> Timestamp {
        Timestamp(secs)
    }

    /// Seconds elapsed from `self` to `other`; negative if `other` is
    /// earlier.
    pub fn elapsed_secs_until(self, other: Timestamp) -> i64 {
        other.0 - self.0
    }

    pub fn format(self) -> String {
        let days = self.0.div_euclid(86400);
        let secs_of_day = self.0.rem_euclid(86400);
        let (year, month, day) = civil_from_days(days);
        let hour = secs_of_day / 3600;
        let minute = (secs_of_day % 3600) / 60;
        let second = secs_of_day % 60;
        format!("{year:04}{month:02}{day:02}.{hour:02}{minute:02}{second:02}")
    }

    pub fn parse(text: &str) -> Result<Timestamp> {
        let (date, time) = text.split_once('.').ok_or_else(|| {
            CryoError::Parse(format!("'{text}' is not a YYYYMMDD.HHMMSS timestamp"))
        })?;

        if date.len() != 8 || time.len() != 6 {
            return Err(CryoError::Parse(format!(
                "'{text}' is not a YYYYMMDD.HHMMSS timestamp"
            )));
        }

        let field = |s: &str| -> Result<i64> {
            s.parse()
                .map_err(|_| CryoError::Parse(format!("'{text}' is not a YYYYMMDD.HHMMSS timestamp")))
        };

        let year = field(&date[0..4])?;
        let month = field(&date[4..6])?;
        let day = field(&date[6..8])?;
        let hour = field(&time[0..2])?;
        let minute = field(&time[2..4])?;
        let second = field(&time[4..6])?;

        let days = days_from_civil(year, month as u32, day as u32);
        let secs = days * 86400 + hour * 3600 + minute * 60 + second;
        Ok(Timestamp(secs))
    }
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_format_and_parse() {
        let cases = [
            (0i64, "19700101.000000"),
            (1_700_000_000, "20231114.221320"),
            (951_782_400, "20000229.000000"), // leap day
            (-1, "19691231.235959"),          // just before the epoch
        ];

        for (secs, text) in cases {
            let ts = Timestamp::from_epoch_secs(secs);
            assert_eq!(ts.format(), text, "formatting {secs}");
            assert_eq!(Timestamp::parse(text).unwrap(), ts, "parsing {text}");
        }
    }

    #[test]
    fn rejects_malformed_text() {
        assert!(Timestamp::parse("not-a-timestamp").is_err());
        assert!(Timestamp::parse("2023111.221320").is_err());
        assert!(Timestamp::parse("20231114221320").is_err());
    }

    #[test]
    fn elapsed_secs_until_is_signed() {
        let a = Timestamp::from_epoch_secs(100);
        let b = Timestamp::from_epoch_secs(160);
        assert_eq!(a.elapsed_secs_until(b), 60);
        assert_eq!(b.elapsed_secs_until(a), -60);
    }
}
