//! Hex view: an `xxd`-like dump for inspecting binary files (read-only copy).

/// At most this many bytes are dumped (the rest is cut off).
pub const MAX_BYTES: usize = 512 * 1024;

/// `00000010  48 65 6c 6c 6f 20 77 6f  72 6c 64 0a              |Hello world.|`
pub fn dump(data: &[u8]) -> String {
    let shown = &data[..data.len().min(MAX_BYTES)];
    let mut out = String::with_capacity(shown.len() * 4 + 64);
    for (i, row) in shown.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", i * 16));
        for k in 0..16 {
            match row.get(k) {
                Some(b) => out.push_str(&format!("{b:02x} ")),
                None => out.push_str("   "),
            }
            if k == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        out.extend(row.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }));
        out.push_str("|\n");
    }
    if data.len() > MAX_BYTES {
        out.push_str(&format!("\n… {} more bytes not shown ({} total)\n", data.len() - MAX_BYTES, data.len()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rows_with_offsets_and_ascii() {
        let d = dump(b"Hello, world!\n\x00\xff");
        let lines: Vec<&str> = d.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("00000000  48 65 6c 6c 6f 2c 20 77  6f 72 6c 64 21 0a 00 ff"), "{}", lines[0]);
        assert!(lines[0].ends_with("|Hello, world!...|"), "{}", lines[0]);
        // The second line's offset and missing bytes are aligned with spaces.
        let d = dump(&[0x41; 20]);
        let l: Vec<&str> = d.lines().collect();
        assert!(l[1].starts_with("00000010  41 41 41 41 "));
        assert!(l[1].ends_with("|AAAA|"));
        assert_eq!(l[0].find('|'), l[1].find('|'), "ASCII column is aligned");
        assert_eq!(dump(&[]), "");
    }

    #[test]
    fn truncates_large_inputs() {
        let d = dump(&vec![7u8; MAX_BYTES + 100]);
        assert!(d.ends_with(&format!("… 100 more bytes not shown ({} total)\n", MAX_BYTES + 100)));
        assert_eq!(d.lines().filter(|l| l.contains('|')).count(), MAX_BYTES / 16);
    }
}
