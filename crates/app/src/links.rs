//! Links in terminal output: URLs and compiler/test locations
//! (`src/main.rs:12:5`, `a.ts(3,7)`, `./x.py:4`). Opened with ⌘-click.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Url(String),
    /// Path (may be relative) and 1-based line/column.
    File {
        path: String,
        line: Option<u32>,
        col: Option<u32>,
    },
}

/// Characters that can't be part of a link (whitespace, quotes, bracket kinds).
fn is_break(c: char) -> bool {
    c.is_whitespace() || c == '\0' || "\"'`<>[]{}|│".contains(c)
}

/// The link at `cells[col]` and the cells it covers. One char per cell
/// (the second cell of a wide char is `'\0'`).
pub fn link_at(cells: &[char], col: usize) -> Option<(Range<usize>, Link)> {
    if col >= cells.len() || is_break(cells[col]) {
        return None;
    }
    let mut start = col;
    while start > 0 && !is_break(cells[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && !is_break(cells[end]) {
        end += 1;
    }
    // Like "(src/a.rs:1:2)" or "at src/a.rs:3.": strip leading and trailing punctuation.
    // `(` only if it isn't part of the "a.ts(3,7)" form.
    while start < end && "(".contains(cells[start]) {
        start += 1;
    }
    loop {
        let last = *cells[start..end].last()?;
        let text: String = cells[start..end].iter().collect();
        let keep_paren = last == ')' && paren_position(&text).is_some();
        if !keep_paren && ".,;:!?)".contains(last) {
            end -= 1;
        } else {
            break;
        }
        if start >= end {
            return None;
        }
    }
    if col < start || col >= end {
        return None;
    }
    let text: String = cells[start..end].iter().collect();
    parse(&text).map(|l| (start..end, l))
}

/// "a.ts(3,7)" → ("a.ts", 3, Some(7)).
fn paren_position(text: &str) -> Option<(&str, u32, Option<u32>)> {
    let inner = text.strip_suffix(')')?;
    let (path, nums) = inner.rsplit_once('(')?;
    let mut it = nums.split(',').map(str::trim);
    let line = it.next()?.parse().ok()?;
    let col = match it.next() {
        Some(c) => Some(c.parse().ok()?),
        None => None,
    };
    (!path.is_empty() && it.next().is_none()).then_some((path, line, col))
}

pub fn parse(text: &str) -> Option<Link> {
    if (text.starts_with("http://") || text.starts_with("https://")) && text.len() > 8 {
        return Some(Link::Url(text.to_string()));
    }
    if text.contains("://") {
        return None; // ftp://, file:// etc. are not opened
    }
    let (path, line, col) = if let Some((p, l, c)) = paren_position(text) {
        (p, Some(l), c)
    } else {
        // Number parts from the end: "path:line:col" or "path:line".
        let mut parts: Vec<&str> = text.split(':').collect();
        let mut nums = Vec::new();
        while parts.len() > 1 && nums.len() < 2 {
            match parts.last().and_then(|p| p.parse::<u32>().ok()) {
                Some(n) => {
                    nums.push(n);
                    parts.pop();
                }
                None => break,
            }
        }
        nums.reverse();
        (&text[..parts.join(":").len()], nums.first().copied(), nums.get(1).copied())
    };
    // Must look like a path: an extension or a directory separator; version numbers ("1.2.3") and
    // times ("12:30") don't count as paths.
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let has_ext = name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()) && ext.chars().any(|c| c.is_ascii_alphabetic())
    });
    let looks_like_path = path.contains(['/', '\\']) || has_ext;
    if path.is_empty() || !looks_like_path || path.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let path = path.strip_prefix("./").unwrap_or(path);
    Some(Link::File { path: path.to_string(), line, col })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(line: &str, needle: &str) -> Option<Link> {
        let cells: Vec<char> = line.chars().collect();
        let col = line.find(needle).map(|b| line[..b].chars().count())?;
        link_at(&cells, col).map(|(_, l)| l)
    }

    fn file(path: &str, line: Option<u32>, col: Option<u32>) -> Option<Link> {
        Some(Link::File { path: path.into(), line, col })
    }

    #[test]
    fn compiler_and_test_locations() {
        assert_eq!(at("  --> crates/app/src/main.rs:12:5", "main"), file("crates/app/src/main.rs", Some(12), Some(5)));
        assert_eq!(at("error: src/lib.rs:3: bad", "lib"), file("src/lib.rs", Some(3), None));
        assert_eq!(at("src/x.ts(14,7): error TS2304", "x.ts"), file("src/x.ts", Some(14), Some(7)));
        assert_eq!(at("  File \"./tool.py\", line 4", "tool"), file("tool.py", None, None));
        assert_eq!(at("at Object.<anonymous> (/home/u/app/index.js:10:15)", "index"), file("/home/u/app/index.js", Some(10), Some(15)));
        assert_eq!(at("see README.md.", "README"), file("README.md", None, None));
        assert_eq!(at("çıktı: ./şablon/ön.rs:7", "ön"), file("şablon/ön.rs", Some(7), None));
    }

    #[test]
    fn windows_compiler_locations_keep_drive_letters_and_backslashes() {
        assert_eq!(at(r"  --> C:\şablon\src\main.rs:12:5", "main"), file(r"C:\şablon\src\main.rs", Some(12), Some(5)));
        assert_eq!(at(r"C:\work\a.ts(14,7): error TS2304", "a.ts"), file(r"C:\work\a.ts", Some(14), Some(7)));
        assert_eq!(at(r"see \\server\share\Makefile:7", "Makefile"), file(r"\\server\share\Makefile", Some(7), None));
    }

    #[test]
    fn urls() {
        assert_eq!(at("Local: http://localhost:5173/", "localhost"), Some(Link::Url("http://localhost:5173/".into())));
        assert_eq!(at("(see https://example.com/a?b=1).", "example"), Some(Link::Url("https://example.com/a?b=1".into())));
        assert_eq!(at("ftp://x.org/f", "x.org"), None);
    }

    #[test]
    fn plain_words_versions_and_times_are_not_links() {
        assert_eq!(at("Compiling app v0.1.0", "Compiling"), None);
        assert_eq!(at("Compiling app v0.1.0", "0.1"), None);
        assert_eq!(at("done at 12:30", "12"), None);
        assert_eq!(at("1.2.3", "1"), None);
        assert_eq!(at("   ", " "), None);
    }

    #[test]
    fn clicking_beside_the_link_is_not_the_link() {
        let line = "(src/a.rs:1)";
        let cells: Vec<char> = line.chars().collect();
        assert_eq!(link_at(&cells, 0), None, "opening parenthesis");
        assert_eq!(link_at(&cells, line.len() - 1), None, "closing parenthesis");
        assert_eq!(link_at(&cells, 1).map(|(r, _)| r), Some(1..11));
    }
}
