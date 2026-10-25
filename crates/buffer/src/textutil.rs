//! Small text helpers (mixed line endings, combining marks).

/// Whether the text contains more than one line-ending style (CRLF with LF, LF with a lone CR, ...).
pub(crate) fn has_mixed_endings(text: &str) -> bool {
    let (mut crlf, mut lf, mut cr) = (0usize, 0usize, 0usize);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    [crlf, lf, cr].iter().filter(|&&n| n > 0).count() > 1
}

/// Combining marks attached to a letter (`e` + U+0301): part of the word, not punctuation (so ⌥← does not stop mid-word in NFD text).
pub(crate) fn is_combining_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}
