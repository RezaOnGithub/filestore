//! Lossless text form of root-relative byte paths.
//!
//! Filesystem names are stored as raw bytes. At the API boundary a path is a
//! string in which valid UTF-8 passes through unchanged, except that `%` is
//! written `%25`; every byte that is not part of valid UTF-8 is written `%XX`.
//! Components are joined with `/`, and the root is the empty string.

use crate::{Error, Result};

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                push_escaped(&mut out, s);
                break;
            }
            Err(e) => {
                let (valid, after) = rest.split_at(e.valid_up_to());
                push_escaped(&mut out, std::str::from_utf8(valid).unwrap());
                let bad = e.error_len().unwrap_or(after.len());
                for b in &after[..bad] {
                    out.push_str(&format!("%{b:02X}"));
                }
                rest = &after[bad..];
            }
        }
    }
    out
}

fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        if c == '%' {
            out.push_str("%25");
        } else {
            out.push(c);
        }
    }
}

pub fn decode(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| Error::Invalid(format!("invalid %-escape in path {s:?}")))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// Decode an API path and check that it is a normalized root-relative path:
/// no leading or trailing `/`, no empty, `.`, or `..` components, no NUL.
pub fn decode_relative(s: &str) -> Result<Vec<u8>> {
    let bytes = decode(s)?;
    validate_relative(&bytes)?;
    Ok(bytes)
}

pub fn validate_relative(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if bytes.contains(&0) {
        return Err(Error::Invalid("path contains NUL".into()));
    }
    for comp in bytes.split(|&b| b == b'/') {
        if comp.is_empty() || comp == b"." || comp == b".." {
            return Err(Error::Invalid(format!(
                "path {:?} must be root-relative and normalized",
                display(bytes)
            )));
        }
    }
    Ok(())
}

pub fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    if parent.is_empty() {
        name.to_vec()
    } else {
        let mut p = Vec::with_capacity(parent.len() + 1 + name.len());
        p.extend_from_slice(parent);
        p.push(b'/');
        p.extend_from_slice(name);
        p
    }
}

pub fn components(path: &[u8]) -> Vec<&[u8]> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split(|&b| b == b'/').collect()
    }
}

pub fn parent(path: &[u8]) -> Option<&[u8]> {
    if path.is_empty() {
        return None;
    }
    Some(match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[..i],
        None => b"",
    })
}

pub fn file_name(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&b| b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Lossy rendering for logs and messages.
pub fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for case in [
            b"plain/name.pdf".as_slice(),
            b"100% done.txt",
            b"bad\xffbyte/\xc3\xa9t\xc3\xa9",
            b"trunc\xc3",
            b"",
        ] {
            let enc = encode(case);
            assert_eq!(decode(&enc).unwrap(), case, "{enc}");
        }
        assert_eq!(encode(b"100% done"), "100%25 done");
        assert_eq!(encode(b"a\xffb"), "a%FFb");
    }

    #[test]
    fn rejects_unnormalized() {
        for bad in ["/a", "a/", "a//b", "a/../b", "./a", ".."] {
            assert!(decode_relative(bad).is_err(), "{bad}");
        }
        assert!(decode_relative("a/b c/d").is_ok());
    }

    #[test]
    fn parents() {
        assert_eq!(parent(b"a/b/c"), Some(b"a/b".as_slice()));
        assert_eq!(parent(b"a"), Some(b"".as_slice()));
        assert_eq!(parent(b""), None);
        assert_eq!(file_name(b"a/b/c"), b"c");
    }
}
