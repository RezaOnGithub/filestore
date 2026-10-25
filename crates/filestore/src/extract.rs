//! Extractors: programs that compute derived metadata from a verified Blob's
//! bytes alone. This prototype ships one streaming extractor, `sniff`, which
//! detects the file format from its leading bytes. Seeking extractors would
//! run on the open handle after the hash; none ship yet.

use serde_json::{Value, json};

pub trait StreamExtractor {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    /// Canonical encoding of every setting that affects the output.
    fn config(&self) -> String;
    fn feed(&mut self, data: &[u8]);
    fn finish(&mut self) -> Option<Value>;
}

pub fn default_stream_extractors() -> Vec<Box<dyn StreamExtractor>> {
    vec![Box::new(Sniff::default())]
}

const HEAD: usize = 8192;

#[derive(Default)]
pub struct Sniff {
    head: Vec<u8>,
    total: u64,
}

impl StreamExtractor for Sniff {
    fn name(&self) -> &'static str {
        "sniff"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn config(&self) -> String {
        "{}".into()
    }

    fn feed(&mut self, data: &[u8]) {
        if self.head.len() < HEAD {
            let take = (HEAD - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..take]);
        }
        self.total += data.len() as u64;
    }

    fn finish(&mut self) -> Option<Value> {
        Some(sniff(&self.head, self.total))
    }
}

fn sniff(h: &[u8], total: u64) -> Value {
    let starts = |p: &[u8]| h.starts_with(p);
    if total == 0 {
        return json!({"format": "empty"});
    }
    if starts(b"%PDF-") {
        let version: String = h[5..h.len().min(12)]
            .iter()
            .take_while(|b| b.is_ascii_digit() || **b == b'.')
            .map(|&b| b as char)
            .collect();
        return json!({"format": "pdf", "version": version});
    }
    if starts(b"\x89PNG\r\n\x1a\n") && h.len() >= 24 {
        let w = u32::from_be_bytes(h[16..20].try_into().unwrap());
        let hgt = u32::from_be_bytes(h[20..24].try_into().unwrap());
        return json!({"format": "png", "width": w, "height": hgt});
    }
    if (starts(b"GIF87a") || starts(b"GIF89a")) && h.len() >= 10 {
        let w = u16::from_le_bytes([h[6], h[7]]);
        let hgt = u16::from_le_bytes([h[8], h[9]]);
        return json!({"format": "gif", "width": w, "height": hgt});
    }
    if starts(b"\xFF\xD8\xFF") {
        return json!({"format": "jpeg"});
    }
    if starts(b"PK\x03\x04") {
        let find = |needle: &[u8]| h.windows(needle.len()).any(|w| w == needle);
        let kind = if find(b"word/") {
            "docx"
        } else if find(b"ppt/") {
            "pptx"
        } else if find(b"xl/") {
            "xlsx"
        } else if find(b"mimetypeapplication/vnd.oasis.opendocument") {
            "opendocument"
        } else if find(b"mimetypeapplication/epub+zip") {
            "epub"
        } else if find(b"[Content_Types].xml") {
            "ooxml"
        } else {
            "zip"
        };
        return json!({"format": kind});
    }
    if starts(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1") {
        return json!({"format": "ole2", "detail": "legacy Office (doc/xls/ppt) or other compound file"});
    }
    for (magic, format) in [
        (b"\x1F\x8B".as_slice(), "gzip"),
        (b"7z\xBC\xAF\x27\x1C", "7z"),
        (b"Rar!\x1A\x07", "rar"),
        (b"\x7FELF", "elf"),
        (b"MZ", "pe"),
        (b"{\\rtf", "rtf"),
        (b"%!PS", "postscript"),
        (b"ID3", "mp3"),
        (b"OggS", "ogg"),
        (b"RIFF", "riff"),
        (b"\x28\xB5\x2F\xFD", "zstd"),
        (b"\xFD7zXZ\x00", "xz"),
        (b"BZh", "bzip2"),
    ] {
        if starts(magic) {
            return json!({"format": format});
        }
    }
    if h.len() >= 12 && &h[4..8] == b"ftyp" {
        let brand = String::from_utf8_lossy(&h[8..12]).trim().to_string();
        return json!({"format": "iso-bmff", "brand": brand});
    }
    let text = h.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(h);
    if is_text(text) {
        let lead: String = String::from_utf8_lossy(&text[..text.len().min(256)])
            .trim_start()
            .chars()
            .take(64)
            .collect::<String>()
            .to_ascii_lowercase();
        let format = if lead.starts_with("<!doctype html") || lead.starts_with("<html") {
            "html"
        } else if lead.starts_with("<?xml") {
            if lead.contains("<svg") { "svg" } else { "xml" }
        } else if lead.starts_with('{') || lead.starts_with('[') {
            "json-like text"
        } else {
            "text"
        };
        return json!({"format": format});
    }
    json!({"format": "binary"})
}

fn is_text(h: &[u8]) -> bool {
    if h.contains(&0) {
        return false;
    }
    match std::str::from_utf8(h) {
        Ok(_) => true,
        // A multi-byte character may be cut at the end of the sniffed head.
        Err(e) => e.error_len().is_none() && h.len() - e.valid_up_to() < 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(bytes: &[u8]) -> Value {
        let mut s = Sniff::default();
        for chunk in bytes.chunks(3) {
            s.feed(chunk);
        }
        s.finish().unwrap()
    }

    #[test]
    fn formats() {
        assert_eq!(run(b"%PDF-1.7\n%..."), json!({"format": "pdf", "version": "1.7"}));
        assert_eq!(run(b""), json!({"format": "empty"}));
        assert_eq!(run(b"hello\nworld")["format"], "text");
        assert_eq!(run(b"  <!DOCTYPE html><html>")["format"], "html");
        assert_eq!(run(b"\x00\x01\x02")["format"], "binary");
    }
}
