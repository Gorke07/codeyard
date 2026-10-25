//! Linked tag editing (HTML/XML/JSX): renaming an opening tag also renames the closing
//! tag, and vice versa. Pure text functions; the editor uses them via `linked_tag_at`.

fn is_name(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')
}

/// `(start, end, is_closing)` if `cursor` (a char index) is inside or at the end of
/// a tag name. The name must start with `<` or `</`.
pub fn name_at(chars: &[char], cursor: usize) -> Option<(usize, usize, bool)> {
    let cursor = cursor.min(chars.len());
    let mut s = cursor;
    while s > 0 && is_name(chars[s - 1]) {
        s -= 1;
    }
    let mut e = cursor;
    while e < chars.len() && is_name(chars[e]) {
        e += 1;
    }
    if s == e {
        return None;
    }
    match (s.checked_sub(1).map(|i| chars[i]), s.checked_sub(2).map(|i| chars[i])) {
        (Some('/'), Some('<')) => Some((s, e, true)),
        (Some('<'), _) => Some((s, e, false)),
        _ => None,
    }
}

/// Whether the tag starts with `<` and ends with `>`: `true` if self-closing (`/>`).
/// `from`: start of the name. A `>` inside quotes doesn't count.
fn self_closing(chars: &[char], from: usize) -> bool {
    let mut quote: Option<char> = None;
    let mut i = from;
    while i < chars.len() && i < from + 20_000 {
        let c = chars[i];
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return i > from && chars[i - 1] == '/',
            _ => {}
        }
        i += 1;
    }
    false
}

fn name_eq(chars: &[char], start: usize, name: &[char]) -> bool {
    chars.len() >= start + name.len() && chars[start..start + name.len()] == *name && chars.get(start + name.len()).is_none_or(|c| !is_name(*c))
}

/// Start of the partner's name. Tags with the same name can nest: depth is counted.
pub fn partner(chars: &[char], start: usize, end: usize, close: bool) -> Option<usize> {
    let name = &chars[start..end];
    if close {
        // Backwards: `</name` increases depth, `<name` (not self-closing) decreases it.
        let mut depth = 0usize;
        if start < 3 {
            return None;
        }
        let mut i = start - 3;
        loop {
            if chars[i] == '<' {
                if chars.get(i + 1) == Some(&'/') && name_eq(chars, i + 2, name) {
                    depth += 1;
                } else if name_eq(chars, i + 1, name) && !self_closing(chars, i + 1) {
                    if depth == 0 {
                        return Some(i + 1);
                    }
                    depth -= 1;
                }
            }
            if i == 0 || start - i > 120_000 {
                return None;
            }
            i -= 1;
        }
    }
    let mut depth = 0usize;
    let mut i = end;
    while i + 1 < chars.len() && i - end < 120_000 {
        if chars[i] == '<' {
            if chars[i + 1] == '/' && name_eq(chars, i + 2, name) {
                if depth == 0 {
                    return Some(i + 2);
                }
                depth -= 1;
            } else if name_eq(chars, i + 1, name) && !self_closing(chars, i + 1) {
                depth += 1;
            }
        }
        i += 1;
    }
    None
}

/// Whether the extension is one of these tag languages.
pub fn applies_to(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref(),
        Some("html" | "htm" | "xhtml" | "xml" | "svg" | "vue" | "jsx" | "tsx" | "xaml" | "php")
    )
}

/// End of the name: where the name characters end, starting from `start`.
pub fn name_end(chars: &[char], start: usize) -> usize {
    let mut e = start.min(chars.len());
    while e < chars.len() && is_name(chars[e]) {
        e += 1;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn finds_names_and_partners() {
        let t = cs("<div class=\"a\"><span>x</span></div>");
        let (s, e, close) = name_at(&t, 2).unwrap();
        assert_eq!((s, e, close), (1, 4, false));
        let p = partner(&t, s, e, close).unwrap();
        assert_eq!(t[p..p + 3].iter().collect::<String>(), "div");
        assert!(p > 20);
        // From closing to opening.
        let (s2, e2, c2) = name_at(&t, p + 1).unwrap();
        assert!(c2);
        assert_eq!(partner(&t, s2, e2, c2), Some(1));
        // No name inside text and attributes.
        assert!(name_at(&t, 8).is_none());
        assert!(name_at(&t, 21).is_none());
    }

    #[test]
    fn nested_same_name_and_self_closing() {
        let t = cs("<a><a/><a>x</a></a>");
        // The outer <a> matches the outer </a>; <a/> doesn't count, an inner <a>..</a> creates depth.
        let p = partner(&t, 1, 2, false).unwrap();
        assert_eq!(p, 17);
        assert_eq!(partner(&t, 17, 18, true), Some(1));
        let inner = partner(&t, 8, 9, false).unwrap();
        assert_eq!(inner, 13);
    }

    #[test]
    fn quotes_and_missing_partners() {
        let t = cs("<p title=\"a>b\">x</p>");
        assert_eq!(partner(&t, 1, 2, false), Some(18));
        assert_eq!(partner(&cs("<br>"), 1, 3, false), None);
        assert_eq!(partner(&cs("text </p>"), 7, 8, true), None);
        assert!(name_at(&[], 0).is_none());
    }

    #[test]
    fn applies_only_to_markup_files() {
        assert!(applies_to("a/b.HTML") && applies_to("x.tsx") && !applies_to("x.rs") && !applies_to("noext"));
    }
}
