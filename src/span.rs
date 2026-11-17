// vim: ts=4 et

//! Interval spans used by both frequency and time-based retention:
//! one or more number-letter pairs (e.g. "15m", "1w3d"), using only
//! m/h/d/w -- per cryo.md, no other units are accepted because
//! they're either too short or ambiguous.

use crate::error::{CryoError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    minutes: u64,
}

impl Span {
    pub fn from_minutes(minutes: u64) -> Span {
        Span { minutes }
    }

    pub fn minutes(self) -> u64 {
        self.minutes
    }

    /// Renders back to the largest-units-first notation it was parsed
    /// from, e.g. 10080+3*1440 minutes displays as "1w3d". Used only
    /// for human-readable output (`cryo status`), not round-tripped
    /// through parsing.
    pub fn format(self) -> String {
        let mut remaining = self.minutes;
        let mut out = String::new();

        for (unit, per_unit) in [('w', 10080), ('d', 1440), ('h', 60), ('m', 1)] {
            let count = remaining / per_unit;
            if count > 0 {
                out.push_str(&count.to_string());
                out.push(unit);
                remaining %= per_unit;
            }
        }

        if out.is_empty() {
            out.push('0');
            out.push('m');
        }

        out
    }

    pub fn parse(text: &str) -> Result<Span> {
        let mut minutes: u64 = 0;
        let mut rest = text;
        let mut saw_pair = false;

        while !rest.is_empty() {
            let digits_len = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());

            if digits_len == 0 {
                return Err(bad_span(text));
            }

            let count: u64 = rest[..digits_len]
                .parse()
                .map_err(|_| bad_span(text))?;

            rest = &rest[digits_len..];

            let mut chars = rest.chars();
            let unit = chars.next().ok_or_else(|| bad_span(text))?;

            let per_unit = match unit {
                'm' => 1,
                'h' => 60,
                'd' => 1440,
                'w' => 10080,
                _ => return Err(bad_span(text)),
            };

            minutes = minutes
                .checked_add(count.checked_mul(per_unit).ok_or_else(|| bad_span(text))?)
                .ok_or_else(|| bad_span(text))?;

            rest = chars.as_str();
            saw_pair = true;
        }

        if !saw_pair {
            return Err(bad_span(text));
        }

        Ok(Span { minutes })
    }
}

fn bad_span(text: &str) -> CryoError {
    CryoError::Parse(format!(
        "'{text}' is not a valid interval (expected number-letter pairs using m/h/d/w)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pairs() {
        assert_eq!(Span::parse("15m").unwrap().minutes(), 15);
        assert_eq!(Span::parse("1h").unwrap().minutes(), 60);
        assert_eq!(Span::parse("1d").unwrap().minutes(), 1440);
        assert_eq!(Span::parse("1w").unwrap().minutes(), 10080);
    }

    #[test]
    fn parses_compound_pairs() {
        assert_eq!(Span::parse("1w3d").unwrap().minutes(), 10080 + 3 * 1440);
        assert_eq!(Span::parse("2d12h").unwrap().minutes(), 2 * 1440 + 12 * 60);
    }

    #[test]
    fn formats_largest_units_first() {
        assert_eq!(Span::from_minutes(15).format(), "15m");
        assert_eq!(Span::from_minutes(10080 + 3 * 1440).format(), "1w3d");
        assert_eq!(Span::from_minutes(0).format(), "0m");
    }

    #[test]
    fn rejects_disallowed_units() {
        for bad in ["1mo", "1y", "1s", "m", "15", ""] {
            assert!(Span::parse(bad).is_err(), "expected '{bad}' to be rejected");
        }
    }
}
