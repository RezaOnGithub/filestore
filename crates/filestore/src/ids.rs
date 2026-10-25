//! Random public identifiers and wall-clock helpers.
//!
//! Public IDs are 128 random bits in lowercase base32 with a type prefix,
//! e.g. `blob_mfrggzdfmztwq2lknnwg23tpoa`. They reveal nothing about content.

use std::time::{SystemTime, UNIX_EPOCH};

const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

pub fn new_public_id(prefix: &str) -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("system randomness is unavailable");
    format!("{prefix}_{}", base32(&bytes))
}

fn base32(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 8 / 5 + 1);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &byte in data {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM UTC` for a Unix time in milliseconds.
pub fn fmt_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_distinct() {
        let a = new_public_id("blob");
        let b = new_public_id("blob");
        assert!(a.starts_with("blob_"));
        assert_eq!(a.len(), "blob_".len() + 26);
        assert_ne!(a, b);
    }

    #[test]
    fn formats_times() {
        assert_eq!(fmt_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(fmt_time(1_761_400_800_000), "2025-10-25 14:00 UTC");
    }

    #[test]
    fn base32_known_vector() {
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
    }
}
