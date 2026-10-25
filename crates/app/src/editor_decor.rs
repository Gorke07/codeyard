//! Pure logic for editor decorations: bracket colors, whitespace markers, color previews,
//! relative line numbers, sticky scope headers. Painting lives in `editor.rs`.

use std::ops::Range;

use syntax::Role;

/// Convert a UTF-16 column to a byte position (`line.len()` if it runs past the line end).
pub fn utf16_to_byte(line: &str, col: u32) -> usize {
    let mut units = 0u32;
    for (i, ch) in line.char_indices() {
        if units >= col {
            return i;
        }
        units += ch.len_utf16() as u32;
    }
    line.len()
}

/// Overlay semantic roles on the syntax runs: `spans` are byte ranges (within the sum of the run
/// lengths). Runs are split where needed.
pub fn apply_roles(runs: Vec<(usize, Option<Role>)>, spans: &[(Range<usize>, Role)]) -> Vec<(usize, Option<Role>)> {
    if spans.is_empty() {
        return runs;
    }
    let total: usize = runs.iter().map(|(n, _)| n).sum();
    // Boundary points instead of a per-byte role array: collect all the boundaries.
    let mut cuts: Vec<usize> = vec![0, total];
    let mut at = 0;
    for (n, _) in &runs {
        at += n;
        cuts.push(at);
    }
    for (r, _) in spans {
        cuts.push(r.start.min(total));
        cuts.push(r.end.min(total));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let base = |pos: usize| {
        let mut at = 0;
        for (n, role) in &runs {
            if pos < at + n {
                return *role;
            }
            at += n;
        }
        None
    };
    let mut out: Vec<(usize, Option<Role>)> = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a == b {
            continue;
        }
        let role = spans.iter().rev().find(|(r, _)| r.start <= a && b <= r.end).map(|(_, role)| Some(*role)).unwrap_or_else(|| base(a));
        match out.last_mut() {
            Some((n, last)) if *last == role => *n += b - a,
            _ => out.push((b - a, role)),
        }
    }
    out
}

/// Brackets in a line: (byte position, color index). Quoted text and line comments
/// are skipped. `depth`: open bracket count at line start; updated to the line-end value on return.
pub fn brackets_in_line(line: &str, depth: &mut i32) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') && !trimmed.starts_with("#[") && !trimmed.starts_with("#!") {
        return out; // shell/Python comment (Rust attributes are told apart by `#[`)
    }
    let mut chars = line.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '`' => quote = Some(ch),
            // Apostrophe: Rust lifetimes ('a) and English text don't count as quotes;
            // only character literals of the form 'x' or '\x' are skipped.
            '\'' => {
                let rest = &line[i + 1..];
                let mut it = rest.chars();
                let skip = match (it.next(), it.next(), it.next()) {
                    (Some('\\'), Some(_), _) => rest.find('\'').map(|j| j + 1),
                    (Some(c), Some('\''), _) if c != '\'' => Some(c.len_utf8() + 1),
                    _ => None,
                };
                if let Some(n) = skip {
                    let target = i + 1 + n;
                    while chars.peek().is_some_and(|&(j, _)| j < target) {
                        chars.next();
                    }
                }
            }
            '/' if chars.peek().is_some_and(|&(_, c)| c == '/') => break,
            '(' | '[' | '{' => {
                out.push((i, (*depth).rem_euclid(3) as usize));
                *depth += 1;
            }
            ')' | ']' | '}' => {
                *depth -= 1;
                out.push((i, (*depth).rem_euclid(3) as usize));
            }
            _ => {}
        }
    }
    out
}

/// Bracket depth at the start of each line (can't go negative: excess closers reset to zero).
pub fn bracket_depths<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<i32> {
    let mut depth = 0i32;
    lines
        .map(|l| {
            let at = depth;
            brackets_in_line(l, &mut depth);
            depth = depth.max(0);
            at
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ws {
    Space,
    Tab,
}

/// Whitespace to show: (byte range, kind). `mode`: "boundary" skips single spaces
/// between words; "all" shows everything; any other value shows none.
pub fn whitespace_marks(text: &str, mode: &str) -> Vec<(Range<usize>, Ws)> {
    if mode != "boundary" && mode != "all" {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    for (k, &(i, ch)) in chars.iter().enumerate() {
        let kind = match ch {
            ' ' => Ws::Space,
            '\t' => Ws::Tab,
            _ => continue,
        };
        if mode == "boundary" && kind == Ws::Space {
            let before = k.checked_sub(1).map(|p| chars[p].1);
            let after = chars.get(k + 1).map(|&(_, c)| c);
            // A single space between words isn't shown.
            if before.is_some_and(|c| !c.is_whitespace()) && after.is_some_and(|c| !c.is_whitespace()) {
                continue;
            }
        }
        out.push((i..i + ch.len_utf8(), kind));
    }
    out
}

/// Unicode characters that are invisible or can be confused with another character (VS Code
/// `editor.unicodeHighlight`). A bidi override hidden in source ("Trojan Source") or a
/// Cyrillic `а` mixed into a Latin identifier is invisible to the eye.
///
/// - Invisible: zero-width spaces, bidi controls, soft hyphen, unusual
///   spaces (NBSP, thin space, ...).
/// - Mixed script: a Cyrillic or Greek letter together with an ASCII letter in the same word.
///
/// Extended Latin letters (Turkish, German, etc.) and single-script Cyrillic/Greek text are not flagged.
/// Returns: (byte range, is invisible).
pub fn suspicious_chars(text: &str) -> Vec<(Range<usize>, bool)> {
    if text.is_ascii() {
        return Vec::new();
    }
    let invisible = |ch: char| {
        matches!(ch,
            '\u{00A0}' | '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}' | '\u{180E}'
            | '\u{2000}'..='\u{200F}' | '\u{2028}'..='\u{202F}' | '\u{205F}'..='\u{206F}' | '\u{3000}' | '\u{3164}'
            | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFB}')
    };
    let confusable_script = |ch: char| matches!(ch, '\u{0370}'..='\u{03FF}' | '\u{0400}'..='\u{04FF}' | '\u{0500}'..='\u{052F}');
    let mut out = Vec::new();
    // Word: a run of letters/digits/underscore; mixed script is decided when the word ends.
    let mut word: Vec<(Range<usize>, char)> = Vec::new();
    let flush = |word: &mut Vec<(Range<usize>, char)>, out: &mut Vec<(Range<usize>, bool)>| {
        if word.iter().any(|(_, c)| c.is_ascii_alphabetic()) {
            out.extend(word.iter().filter(|(_, c)| confusable_script(*c)).map(|(r, _)| (r.clone(), false)));
        }
        word.clear();
    };
    for (i, ch) in text.char_indices() {
        let range = i..i + ch.len_utf8();
        if invisible(ch) {
            flush(&mut word, &mut out);
            out.push((range, true));
        } else if ch.is_alphanumeric() || ch == '_' {
            word.push((range, ch));
        } else {
            flush(&mut word, &mut out);
        }
    }
    flush(&mut word, &mut out);
    out.sort_by_key(|(r, _)| r.start);
    out
}

/// Line number text: "on" absolute, "off" none, "relative" absolute on the cursor line,
/// elsewhere the distance to the cursor line.
pub fn line_number(mode: &str, line: usize, cursor_line: usize) -> Option<String> {
    match mode {
        "off" => None,
        "relative" if line != cursor_line => Some(line.abs_diff(cursor_line).to_string()),
        _ => Some((line + 1).to_string()),
    }
}

/// Sticky headers: header lines of the scopes enclosing line `first`, outermost first,
/// at most `max`. `indent(l)`: indent of the line (`None` for an empty line). Line `first` itself
/// doesn't count as a header (its scope starts there).
pub fn sticky_headers(first: usize, max: usize, indent: impl Fn(usize) -> Option<usize>) -> Vec<usize> {
    // First line with an indent: empty lines don't define a scope.
    let Some(mut level) = (first..first + 200).find_map(&indent) else { return Vec::new() };
    let mut out = Vec::new();
    let mut i = first;
    let floor = first.saturating_sub(5000);
    while i > floor && level > 0 {
        i -= 1;
        if let Some(l) = indent(i).filter(|&l| l < level) {
            out.push(i);
            level = l;
        }
    }
    out.reverse();
    // If too deep, the innermost ones are kept.
    let skip = out.len().saturating_sub(max);
    out.split_off(skip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn depths(src: &str) -> Vec<(usize, usize)> {
        let mut d = 0;
        src.lines().flat_map(|l| brackets_in_line(l, &mut d)).collect()
    }

    #[test]
    fn brackets_get_colors_by_depth_and_cycle() {
        // ( [ { ( -> 0 1 2 0, closers get the same color.
        let got = depths("([{()}])");
        assert_eq!(got.iter().map(|b| b.1).collect::<Vec<_>>(), [0, 1, 2, 0, 0, 2, 1, 0]);
        assert_eq!(got.iter().map(|b| b.0).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn brackets_in_strings_and_comments_are_skipped() {
        assert!(depths(r#"let s = "(not [a bracket"; // ) ]"#).is_empty());
        assert_eq!(depths("f('(' , x)").len(), 2, "character literal is skipped");
        assert_eq!(depths("fn f<'a>(x: &'a str)").len(), 2, "lifetime is not a quote");
        assert!(depths("# comment (x)").is_empty());
        assert_eq!(depths("#[derive(Debug)]").len(), 4, "Rust attribute is not a comment");
    }

    #[test]
    fn depth_carries_across_lines_and_never_goes_negative() {
        let lines = ["fn f() {", "    if x {", "        y();", "    }", "}", ")"];
        assert_eq!(bracket_depths(lines.iter().copied()), [0, 1, 2, 2, 1, 0]);
        assert_eq!(bracket_depths(["}", "}", "x"].iter().copied()), [0, 0, 0]);
    }

    #[test]
    fn whitespace_modes() {
        let t = "a b  c\t d ";
        assert_eq!(whitespace_marks(t, "none"), []);
        let all = whitespace_marks(t, "all");
        assert_eq!(all.len(), 6);
        assert_eq!(all[2], (4..5, Ws::Space));
        assert_eq!(all[3], (6..7, Ws::Tab));
        // boundary: the single space between "a b" is hidden; double space, tab, trailing are shown.
        let b: Vec<usize> = whitespace_marks(t, "boundary").iter().map(|(r, _)| r.start).collect();
        assert_eq!(b, [3, 4, 6, 7, 9], "{b:?}");
        assert_eq!(whitespace_marks("  x", "boundary").len(), 2, "leading indent is shown");
    }

    #[test]
    fn line_numbers_by_mode() {
        assert_eq!(line_number("on", 4, 0).as_deref(), Some("5"));
        assert_eq!(line_number("off", 4, 0), None);
        assert_eq!(line_number("relative", 4, 4).as_deref(), Some("5"), "cursor line is absolute");
        assert_eq!(line_number("relative", 1, 4).as_deref(), Some("3"));
        assert_eq!(line_number("relative", 9, 4).as_deref(), Some("5"));
    }

    #[test]
    fn sticky_headers_are_the_enclosing_scopes() {
        let src = ["mod a {", "    fn f() {", "        let x = 1;", "", "        let y = 2;", "    }", "}"];
        let indent = |l: usize| src.get(l).and_then(|s| if s.trim().is_empty() { None } else { Some(s.len() - s.trim_start().len()) });
        assert_eq!(sticky_headers(4, 3, indent), [0, 1], "inside let y: mod a, fn f");
        assert_eq!(sticky_headers(2, 3, indent), [0, 1]);
        assert_eq!(sticky_headers(1, 3, indent), [0], "line fn f is not its own header");
        assert_eq!(sticky_headers(0, 3, indent), [] as [usize; 0]);
        assert_eq!(sticky_headers(4, 1, indent), [1], "innermost ones are kept");
        // Starting from an empty line gives the scope of the next non-empty line.
        assert_eq!(sticky_headers(3, 3, indent), [0, 1]);
    }

    #[test]
    fn semantic_roles_overlay_and_utf16_columns() {
        use syntax::Role::*;
        // 10 bytes: [0..4) Keyword, [4..10) plain. Semantic: [2..6) Function.
        let runs = vec![(4, Some(Keyword)), (6, None)];
        let out = apply_roles(runs.clone(), &[(2..6, Function)]);
        assert_eq!(out, vec![(2, Some(Keyword)), (4, Some(Function)), (4, None)]);
        assert_eq!(out.iter().map(|(n, _)| n).sum::<usize>(), 10);
        assert_eq!(apply_roles(runs.clone(), &[]), runs);
        // Out-of-range and overlapping overflows are clipped; the same role merges.
        assert_eq!(apply_roles(vec![(3, None)], &[(0..2, Type), (2..9, Type)]), vec![(3, Some(Type))]);
        // UTF-16: "é" is 1 unit, "😀" is 2 units (4 bytes).
        assert_eq!(utf16_to_byte("aé😀b", 0), 0);
        assert_eq!(utf16_to_byte("aé😀b", 2), 3);
        assert_eq!(utf16_to_byte("aé😀b", 4), 7);
        assert_eq!(utf16_to_byte("aé😀b", 99), "aé😀b".len());
    }

    #[test]
    fn suspicious_chars_flag_invisible_and_mixed_script() {
        // ASCII: fast path, nothing found.
        assert!(suspicious_chars("let x = 1;").is_empty());
        // Turkish and German Latin letters are harmless.
        assert!(suspicious_chars("İğne ışık çöp Straße").is_empty());
        // Zero-width space and bidi override: invisible.
        let t = "a\u{200B}b\u{202E}c";
        assert_eq!(suspicious_chars(t), vec![(1..4, true), (5..8, true)]);
        // NBSP counts as invisible.
        assert_eq!(suspicious_chars("a\u{00A0}b"), vec![(1..3, true)]);
        // Cyrillic "а" (U+0430) in a Latin identifier: mixed script, only the Cyrillic letter is flagged.
        assert_eq!(suspicious_chars("p\u{0430}ssword"), vec![(1..3, false)]);
        // Single-script Cyrillic and Greek text is harmless.
        assert!(suspicious_chars("привет мир, καλημέρα").is_empty());
        // Word boundary: a Latin word + space + a Cyrillic word isn't counted as mixed.
        assert!(suspicious_chars("main привет").is_empty());
        // Returned in order.
        let mixed = suspicious_chars("a\u{200B} p\u{0430}");
        assert!(mixed.windows(2).all(|w| w[0].0.start < w[1].0.start), "{mixed:?}");
    }
}
