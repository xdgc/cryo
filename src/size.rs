// vim: ts=4 et

//! Byte sizes for `cryo:limit`/`cryo:reserve`: a bare number of
//! bytes, or a number with a conventional size suffix -- lowercase is
//! decimal (order 10^3, e.g. g=gigabytes), uppercase is binary (order
//! 2^10, e.g. G=gibibytes). See cryo.md, "Snapshot behaviors".

use crate::error::{CryoError, Result};

pub fn parse(text: &str) -> Result<u64> {
    let last = text.chars().last().ok_or_else(|| bad_size(text))?;

    if last.is_ascii_digit() {
        return text.parse().map_err(|_| bad_size(text));
    }

    let split_at = text.len() - last.len_utf8();
    let count: u64 = text[..split_at].parse().map_err(|_| bad_size(text))?;

    let multiplier: u64 = match last {
        'b' | 'B' => 1,
        'k' => 1_000,
        'K' => 1024,
        'm' => 1_000_000,
        'M' => 1024 * 1024,
        'g' => 1_000_000_000,
        'G' => 1024 * 1024 * 1024,
        't' => 1_000_000_000_000,
        'T' => 1024u64.pow(4),
        'p' => 1_000_000_000_000_000,
        'P' => 1024u64.pow(5),
        _ => return Err(bad_size(text)),
    };

    count.checked_mul(multiplier).ok_or_else(|| bad_size(text))
}

fn bad_size(text: &str) -> CryoError {
    CryoError::Parse(format!("'{text}' is not a valid size"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_bytes() {
        assert_eq!(parse("1024").unwrap(), 1024);
    }

    #[test]
    fn parses_decimal_suffixes() {
        assert_eq!(parse("10g").unwrap(), 10_000_000_000);
        assert_eq!(parse("1k").unwrap(), 1_000);
    }

    #[test]
    fn parses_binary_suffixes() {
        assert_eq!(parse("10G").unwrap(), 10 * 1024 * 1024 * 1024);
        assert_eq!(parse("1K").unwrap(), 1024);
    }

    #[test]
    fn rejects_malformed_sizes() {
        for bad in ["", "g", "10x", "-5g"] {
            assert!(parse(bad).is_err(), "expected '{bad}' to be rejected");
        }
    }
}
