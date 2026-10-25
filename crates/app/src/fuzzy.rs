//! Fuzzy matching for short lists (commands, themes). Rule: letters must appear in order;
//! word start +10, adjacent +5, first letter +3, each skipped character -1 (at most -20).
//! Turkish folding: i/ı/İ/I are equivalent, ş/ç/ğ/ö/ü → s/c/g/o/u.
//! `nucleo` handles thousands of file paths; this greedy version is for short labels.

/// Turkish folding + lowercase (one char per char: indices are preserved).
pub fn fold(ch: char) -> char {
    match ch {
        'İ' | 'I' | 'ı' => 'i',
        'Ş' | 'ş' => 's',
        'Ç' | 'ç' => 'c',
        'Ğ' | 'ğ' => 'g',
        'Ö' | 'ö' => 'o',
        'Ü' | 'ü' => 'u',
        c => c.to_lowercase().next().unwrap_or(c),
    }
}

fn is_word_start(chars: &[char], i: usize) -> bool {
    i == 0 || matches!(chars[i - 1], ' ' | ':' | '/' | '.' | '_' | '-' | '(')
}

/// (score, matched char indices). `None` if there is no match; an empty query matches everything.
pub fn score(query: &str, label: &str) -> Option<(i32, Vec<usize>)> {
    let chars: Vec<char> = label.chars().collect();
    let mut hits = Vec::new();
    let mut score = 0;
    let mut from = 0;
    for q in query.chars().filter(|c| !c.is_whitespace()).map(fold) {
        let i = (from..chars.len()).find(|&i| fold(chars[i]) == q)?;
        if is_word_start(&chars, i) {
            score += 10;
        }
        if hits.last() == Some(&(i.wrapping_sub(1))) {
            score += 5;
        }
        if i == 0 {
            score += 3;
        }
        score -= (i - from).min(20) as i32;
        hits.push(i);
        from = i + 1;
    }
    Some((score, hits))
}

/// Converts char indices to byte ranges (for text highlighting).
pub fn hit_ranges(label: &str, hits: &[usize]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    for (ci, (bi, ch)) in label.char_indices().enumerate() {
        if hits.contains(&ci) {
            out.push(bi..bi + ch.len_utf8());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_cases_as_js_preview() {
        assert_eq!(score("gpk", "Görünüm: Paneli Aç/Kapat").unwrap().1, [0, 9, 19]);
        assert_eq!(score("gorunum", "Görünüm: Kenar").unwrap().1, [0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(score("ışık", "IŞIK modu").unwrap().1, [0, 1, 2, 3]);
        assert!(score("xyz", "Dosya: Kaydet").is_none());
        let short = score("kay", "Dosya: Kaydet").unwrap().0;
        let long = score("kay", "Dosya: Tümünü Kaydet").unwrap().0;
        assert!(short > long);
    }

    #[test]
    fn theme_names() {
        assert!(score("hcd", "High Contrast Dark").is_some());
        let fj = score("fj", "Fjord").unwrap().0;
        let other = score("fj", "Midnight (fj)").map_or(i32::MIN, |s| s.0);
        assert!(fj > other);
    }

    #[test]
    fn hit_ranges_are_byte_ranges() {
        assert_eq!(hit_ranges("ağ", &[1]), [1..3]);
    }
}
