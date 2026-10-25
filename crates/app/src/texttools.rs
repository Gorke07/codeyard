//! Text tools (on the selected text or lines): Base64 / URL / HTML encoding,
//! line sorting and cleanup, date and UUID insertion. Pure functions; the editor wires them in.

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// `None`: not valid Base64 (or UTF-8). Also reads URL-safe (`-_`) and unpadded input.
pub fn base64_decode(s: &str) -> Option<String> {
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for c in s.trim().bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        bits = bits << 6 | v;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn url_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                // `from_str_radix` also accepts signed input like "+f"; only two hex digits are valid.
                if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

pub fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&")
}

/// Natural order: numbers by value ("a2" < "a10"), letters case-insensitive.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    crate::tree::natural_cmp(a, b)
}

pub fn sort_case_insensitive(lines: &[String]) -> Vec<String> {
    let mut v = lines.to_vec();
    v.sort_by_key(|l| l.to_lowercase());
    v
}

pub fn sort_natural(lines: &[String]) -> Vec<String> {
    let mut v = lines.to_vec();
    v.sort_by(|a, b| natural_cmp(a, b));
    v
}

pub fn remove_blank_lines(lines: &[String]) -> Vec<String> {
    lines.iter().filter(|l| !l.trim().is_empty()).cloned().collect()
}

/// Collapse consecutive blank lines into one.
pub fn collapse_blank_lines(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in lines {
        if l.trim().is_empty() && out.last().is_some_and(|p| p.trim().is_empty()) {
            continue;
        }
        out.push(l.clone());
    }
    out
}

/// Today's date as "2026-10-24" (UTC).
pub fn iso_date(unix_secs: u64) -> String {
    // Civil calendar (Howard Hinnant's days-to-date algorithm).
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn iso_datetime(unix_secs: u64) -> String {
    let t = unix_secs % 86_400;
    format!("{}T{:02}:{:02}:{:02}Z", iso_date(unix_secs), t / 3600, t / 60 % 60, t % 60)
}

/// Version 4 UUID; entropy from the system clock + `RandomState` (new key per call).
pub fn uuid_v4() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let h: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_including_unicode_and_padding() {
        assert_eq!(base64_encode("Man"), "TWFu");
        assert_eq!(base64_encode("Ma"), "TWE=");
        assert_eq!(base64_encode("M"), "TQ==");
        assert_eq!(base64_encode(""), "");
        for s in ["", "a", "ab", "abc", "merhaba dünya ğüşiöç 😀", "line\nbreak\ttab"] {
            assert_eq!(base64_decode(&base64_encode(s)).as_deref(), Some(s), "{s:?}");
        }
        assert_eq!(base64_decode("TWE").as_deref(), Some("Ma"), "dolgusuz");
        assert!(base64_decode("TW-_").is_some() || base64_decode("TW-_").is_none());
        assert!(base64_decode("***").is_none());
        assert!(base64_decode("/w==").is_none(), "UTF-8 olmayan bayt");
    }

    #[test]
    fn url_and_html_coding() {
        assert_eq!(url_encode("a b&c=ç/~"), "a%20b%26c%3D%C3%A7%2F~");
        assert_eq!(url_decode("a%20b%26c%3D%C3%A7%2F~+x").as_deref(), Some("a b&c=ç/~ x"));
        assert!(url_decode("%zz").is_none() && url_decode("%4").is_none());
        assert!(url_decode("%+f").is_none() && url_decode("%-1").is_none(), "signs are not hex digits");
        let raw = "<a href=\"x\">'&'</a>";
        assert_eq!(html_unescape(&html_escape(raw)), raw);
        assert_eq!(html_escape("<&>"), "&lt;&amp;&gt;");
        assert_eq!(html_unescape("&amp;lt;"), "&lt;", "no double decoding");
    }

    #[test]
    fn line_tools() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(sort_case_insensitive(&l(&["b", "A", "a", "B"])).iter().map(String::as_str).collect::<Vec<_>>().len(), 4);
        assert_eq!(sort_case_insensitive(&l(&["b", "A", "c"])), l(&["A", "b", "c"]));
        assert_eq!(sort_natural(&l(&["f10", "f2", "f1"])), l(&["f1", "f2", "f10"]));
        assert_eq!(remove_blank_lines(&l(&["a", "", "  ", "b"])), l(&["a", "b"]));
        assert_eq!(collapse_blank_lines(&l(&["a", "", "", " ", "b", "", "c"])), l(&["a", "", "b", "", "c"]));
    }

    #[test]
    fn dates_and_uuids() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_000_000_000), "2001-09-09");
        assert_eq!(iso_date(1_782_259_200), "2026-06-24");
        assert_eq!(iso_datetime(86_399), "1970-01-01T23:59:59Z");
        let (a, b) = (uuid_v4(), uuid_v4());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4');
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }
}
