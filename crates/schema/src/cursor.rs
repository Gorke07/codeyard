//! Cursor context: even in a half-typed (invalid) document, finds the path we're in, whether a key or a value
//! is being typed, and which keys the object already has. Completion uses this; no full parse needed.

use std::collections::HashSet;

use crate::walk::Seg;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Slot {
    Key,
    Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctx {
    pub path: Vec<Seg>,
    pub slot: Slot,
    /// Prefix typed so far (inside quotes or a bare word).
    pub prefix: String,
    /// Start of the range to replace (byte): the opening quote if quoted, else the start of the word.
    pub start: usize,
    /// End of the range to replace: the cursor, plus an auto-closed `"` right after it if present.
    pub end: usize,
    pub quoted: bool,
    /// Keys already in this object (so they aren't suggested again).
    pub existing: HashSet<String>,
    /// Whether `:` already follows the key (so renaming doesn't add `: ` again).
    pub colon_follows: bool,
    /// Line indent (YAML only; 0 for JSON).
    pub indent: usize,
}

// --- JSON ---

#[derive(Debug)]
struct Frame {
    obj: bool,
    key: Option<String>,
    index: usize,
    keys: HashSet<String>,
    expect: Expect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Key,
    Colon,
    Value,
    CommaOrEnd,
}

pub fn json_ctx(text: &str, offset: usize) -> Option<Ctx> {
    let b = text.as_bytes();
    let offset = offset.min(b.len());
    let mut frames: Vec<Frame> = Vec::new();
    let mut i = 0;
    // Open string: (start byte, contents).
    let mut open: Option<(usize, Vec<u8>)> = None;
    let mut bare: Option<usize> = None;
    while i < offset {
        let c = b[i];
        if let Some((_, buf)) = &mut open {
            match c {
                b'\\' => {
                    if i + 1 < offset {
                        buf.push(b[i + 1]);
                    }
                    i += 2;
                    continue;
                }
                b'"' => {
                    let (_, s) = open.take().expect("open");
                    let s = String::from_utf8_lossy(&s).into_owned();
                    if let Some(f) = frames.last_mut() {
                        match (f.obj, f.expect) {
                            (true, Expect::Key) => {
                                f.keys.insert(s.clone());
                                f.key = Some(s);
                                f.expect = Expect::Colon;
                            }
                            _ => f.expect = Expect::CommaOrEnd,
                        }
                    }
                }
                _ => buf.push(c),
            }
            i += 1;
            continue;
        }
        if bare.is_some() && !(c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+' | b'_')) {
            bare = None;
            if let Some(f) = frames.last_mut() {
                f.expect = Expect::CommaOrEnd;
            }
        }
        match c {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < offset && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(offset, |n| i + n + 4).min(offset.max(i + 2));
                continue;
            }
            b'"' => open = Some((i, Vec::new())),
            b'{' | b'[' => {
                frames.push(Frame { obj: c == b'{', key: None, index: 0, keys: HashSet::new(), expect: if c == b'{' { Expect::Key } else { Expect::Value } });
            }
            b'}' | b']' => {
                frames.pop();
                if let Some(f) = frames.last_mut() {
                    f.expect = Expect::CommaOrEnd;
                }
            }
            b':' => {
                if let Some(f) = frames.last_mut() {
                    f.expect = Expect::Value;
                }
            }
            b',' => {
                if let Some(f) = frames.last_mut() {
                    if f.obj {
                        f.expect = Expect::Key;
                        f.key = None;
                    } else {
                        f.index += 1;
                        f.expect = Expect::Value;
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.') => {
                bare.get_or_insert(i);
            }
            _ => {}
        }
        i += 1;
    }
    let top = frames.last()?;
    let (start, quoted, prefix) = match (&open, bare) {
        (Some((s, buf)), _) => (*s, true, String::from_utf8_lossy(buf).into_owned()),
        (None, Some(s)) => (s, false, text[s..offset].to_string()),
        _ => (offset, false, String::new()),
    };
    let slot = match (top.obj, top.expect, quoted) {
        (true, Expect::Key, _) => Slot::Key,
        // While typing a key (open string) or `{ "a": 1, "b|`.
        (true, Expect::CommaOrEnd | Expect::Colon, _) if !quoted && bare.is_none() => return None,
        (true, Expect::Value, _) => Slot::Value,
        (false, Expect::Value, _) => Slot::Value,
        (true, Expect::Colon | Expect::CommaOrEnd, true) => return None,
        _ => return None,
    };
    let mut path: Vec<Seg> = Vec::new();
    for f in &frames[..frames.len() - 1] {
        path.push(if f.obj { Seg::Key(f.key.clone()?) } else { Seg::Index(f.index) });
    }
    if slot == Slot::Value {
        path.push(if top.obj { Seg::Key(top.key.clone()?) } else { Seg::Index(top.index) });
    }
    // Include an auto-closed `"` right after the open quote in the range.
    let end = if quoted && b.get(offset) == Some(&b'"') { offset + 1 } else { offset };
    let after = text[end..].trim_start_matches([' ', '\t']);
    Some(Ctx { path, slot, prefix, start, end, quoted, existing: top.keys.clone(), colon_follows: after.starts_with(':'), indent: 0 })
}

// --- YAML ---

#[derive(Debug)]
enum Scope {
    Map { indent: usize, key: Option<String>, keys: HashSet<String> },
    Seq { indent: usize },
}

/// Leading space count and the content after stripping "- " prefixes: (dash indents, content indent, content).
fn split_line(line: &str) -> (Vec<usize>, usize, &str) {
    let mut dashes = Vec::new();
    let mut col = line.len() - line.trim_start_matches(' ').len();
    let mut rest = &line[col..];
    while let Some(r) = rest.strip_prefix("- ").or_else(|| (rest == "-").then_some("")) {
        dashes.push(col);
        let skipped = rest.len() - r.len();
        let extra = r.len() - r.trim_start_matches(' ').len();
        col += skipped + extra;
        rest = r.trim_start_matches(' ');
        if rest.is_empty() {
            break;
        }
    }
    (dashes, col, rest)
}

/// Key of a `key:` or `key: value` line and whether the value is empty. Quoted keys are supported.
fn key_of(content: &str) -> Option<(String, bool)> {
    let content = content.trim_end();
    let (key, rest) = if let Some(q) = content.strip_prefix('"').or_else(|| content.strip_prefix('\'')) {
        let quote = content.as_bytes()[0] as char;
        let end = q.find(quote)?;
        (q[..end].to_string(), q[end + 1..].strip_prefix(':')?)
    } else {
        let mut at = None;
        let bytes = content.as_bytes();
        for (i, &c) in bytes.iter().enumerate() {
            if c == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
                at = Some(i);
                break;
            }
            if c == b'#' && i > 0 && bytes[i - 1] == b' ' {
                break;
            }
        }
        let at = at?;
        (content[..at].to_string(), &content[at + 1..])
    };
    let value = rest.trim();
    Some((key, value.is_empty() || value.starts_with('#') || matches!(value, "|" | ">" | "|-" | ">-" | "|+" | ">+")))
}

pub fn yaml_ctx(text: &str, offset: usize) -> Option<Ctx> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |n| n + 1);
    let line_end = text[offset..].find('\n').map_or(text.len(), |n| offset + n);
    let line_no = text[..line_start].matches('\n').count();
    let lines: Vec<&str> = text.lines().collect();
    let mut stack: Vec<Scope> = Vec::new();
    for (n, raw) in lines.iter().enumerate().take(line_no + 1) {
        // On the cursor line only the text left of the cursor counts.
        let line: &str = if n == line_no { &text[line_start..offset] } else { raw };
        let trimmed = line.trim_start_matches(' ');
        if n != line_no && (trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "...") {
            continue;
        }
        if n != line_no && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
            continue;
        }
        let (dashes, content_col, content) = split_line(line);
        let first = line.len() - trimmed.len();
        // Indent decreased: deeper scopes close. A sequence may sit at its parent key's indent (`steps:` / `- run:`).
        let starts_with_dash = !dashes.is_empty();
        while let Some(top) = stack.last() {
            let close = match top {
                Scope::Map { indent, .. } => *indent > first,
                Scope::Seq { indent } => *indent > first || (*indent == first && !starts_with_dash),
            };
            if close {
                stack.pop();
            } else {
                break;
            }
        }
        for d in &dashes {
            // The item's own scope; a matching earlier sequence scope is cleared for the reopened item.
            if let Some(Scope::Seq { indent }) = stack.last()
                && *indent == *d
            {
                // Next item of the same sequence: scopes inside the item were already closed above.
                continue;
            }
            stack.push(Scope::Seq { indent: *d });
        }
        if content.is_empty() {
            continue;
        }
        // Map scope: the existing map at this indent, or a new one.
        let need_new = !matches!(stack.last(), Some(Scope::Map { indent, .. }) if *indent == content_col);
        if need_new {
            // Content right after the dash is a new map (a new sequence item).
            stack.push(Scope::Map { indent: content_col, key: None, keys: HashSet::new() });
        }
        // Excluding the cursor line: its key isn't written yet or is still being typed.
        if let Some((k, empty)) = key_of(content)
            && n != line_no
            && let Some(Scope::Map { key, keys, .. }) = stack.last_mut()
        {
            keys.insert(k.clone());
            *key = empty.then_some(k);
        }
    }
    // The cursor line.
    let line = &text[line_start..offset];
    let (_, content_col, content) = split_line(line);
    let after_line = &text[offset..line_end];
    // In the value part? Left of the cursor there's `key:` + space.
    let value_slot = key_of_prefix(content);
    let (slot, prefix_start, key_here) = match &value_slot {
        Some((k, value_start)) => (Slot::Value, content_col + value_start, Some(k.clone())),
        None => {
            // Key part: start of the word.
            let word = content.rsplit([' ', '"', '\'']).next().unwrap_or("");
            (Slot::Key, offset - line_start - word.len(), None)
        }
    };
    // Path: the scopes' keys (minus the innermost map, whose key is our sibling) and sequence indices.
    let mut path: Vec<Seg> = Vec::new();
    let mut existing = HashSet::new();
    let own_indent = content_col;
    // The cursor line's own map: the one on the stack at this indent, else a new one (child of the parent key).
    let mut scopes: Vec<&Scope> = stack.iter().collect();
    // Close whatever the cursor line's indent closes.
    let first = line.len() - line.trim_start_matches(' ').len();
    let starts_with_dash = line.trim_start_matches(' ').starts_with("- ") || line.trim() == "-";
    while let Some(top) = scopes.last() {
        let close = match top {
            Scope::Map { indent, .. } => *indent > own_indent && *indent > first,
            Scope::Seq { indent } => *indent > first || (*indent == first && !starts_with_dash),
        };
        if close {
            scopes.pop();
        } else {
            break;
        }
    }
    let own_map = matches!(scopes.last(), Some(Scope::Map { indent, .. }) if *indent == own_indent);
    for (n, s) in scopes.iter().enumerate() {
        match s {
            Scope::Seq { .. } => path.push(Seg::Index(0)),
            Scope::Map { key, keys, .. } => {
                let last_own = n + 1 == scopes.len() && own_map;
                if last_own {
                    existing = keys.clone();
                } else if let Some(k) = key {
                    path.push(Seg::Key(k.clone()));
                } else {
                    // Intermediate map with no parent key (invalid structure): path is ambiguous.
                    return None;
                }
            }
        }
    }
    if let Some(k) = key_here.clone() {
        path.push(Seg::Key(k));
    }
    let prefix = text[line_start + prefix_start..offset].to_string();
    let quoted = prefix.starts_with('"') || prefix.starts_with('\'');
    let prefix_clean = prefix.trim_start_matches(['"', '\'']).to_string();
    Some(Ctx {
        path,
        slot,
        prefix: prefix_clean,
        start: line_start + prefix_start,
        end: offset,
        quoted,
        existing,
        colon_follows: after_line.trim_start().starts_with(':'),
        indent: own_indent,
    })
}

/// `key: typ|`: key and value start columns in the content left of the cursor.
fn key_of_prefix(content: &str) -> Option<(String, usize)> {
    let bytes = content.as_bytes();
    let mut in_quote: Option<u8> = None;
    for (i, &c) in bytes.iter().enumerate() {
        match (in_quote, c) {
            (Some(q), c) if c == q => in_quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') if i == 0 => in_quote = Some(c),
            (None, b':') if i + 1 < bytes.len() && bytes[i + 1] == b' ' => {
                let key = content[..i].trim_matches(['"', '\'']).to_string();
                let rest = &content[i + 1..];
                let skipped = rest.len() - rest.trim_start_matches(' ').len();
                return Some((key, i + 1 + skipped));
            }
            (None, b':') if i + 1 == bytes.len() => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> (String, usize) {
        let i = text.find('|').expect("cursor marker");
        (text.replacen('|', "", 1), i)
    }

    fn json(marked: &str) -> Option<Ctx> {
        let (t, i) = at(marked);
        json_ctx(&t, i)
    }

    fn yaml(marked: &str) -> Option<Ctx> {
        let (t, i) = at(marked);
        yaml_ctx(&t, i)
    }

    fn k(s: &str) -> Seg {
        Seg::Key(s.into())
    }

    #[test]
    fn json_key_positions() {
        let c = json("{\n  \"name\": \"x\",\n  \"|\n}").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone(), c.quoted, c.prefix.as_str()), (Slot::Key, vec![], true, ""));
        assert!(c.existing.contains("name"));
        let c = json("{\"a\": {\"b\": 1, \"c|\"}}").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone(), c.prefix.as_str()), (Slot::Key, vec![k("a")], "c"));
        assert!(c.existing.contains("b"));
        assert_eq!(c.end, c.start + 3, "opening quote, `c` and the auto-closed quote are all replaced");
        let c = json("{\"a\": 1, |}").unwrap();
        assert_eq!((c.slot.clone(), c.quoted, c.start, c.end), (Slot::Key, false, 9, 9));
        let c = json("{ |").unwrap();
        assert_eq!(c.slot, Slot::Key);
        assert!(json("{\"a\" |").is_none(), "after a key and before the colon nothing is offered");
    }

    #[test]
    fn json_value_positions_inside_arrays_and_nested_objects() {
        let c = json("{\"jobs\": {\"build\": {\"runs-on\": \"|\"}}}").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone()), (Slot::Value, vec![k("jobs"), k("build"), k("runs-on")]));
        assert!(c.quoted);
        let c = json("{\"steps\": [{\"run\": 1}, {\"na|").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone()), (Slot::Key, vec![k("steps"), Seg::Index(1)]));
        let c = json("{\"list\": [1, 2, |]}").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone()), (Slot::Value, vec![k("list"), Seg::Index(2)]));
        let c = json("{\"private\": tr|}").unwrap();
        assert_eq!((c.slot.clone(), c.quoted, c.prefix.as_str()), (Slot::Value, false, "tr"));
        // Comments and strings containing braces do not confuse the scan.
        let c = json("{ // {\n \"a\": \"}{\", \"b\": |}").unwrap();
        assert_eq!(c.path, vec![k("b")]);
    }

    #[test]
    fn yaml_nested_keys_and_values() {
        let c = yaml("name: ci\njobs:\n  build:\n    runs-on: ubuntu-latest\n    |").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone()), (Slot::Key, vec![k("jobs"), k("build")]));
        assert!(c.existing.contains("runs-on"));
        let c = yaml("jobs:\n  build:\n    runs-on: ub|").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone(), c.prefix.as_str()), (Slot::Value, vec![k("jobs"), k("build"), k("runs-on")], "ub"));
        let c = yaml("name: |").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone()), (Slot::Value, vec![k("name")]));
        let c = yaml("na|").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone(), c.prefix.as_str()), (Slot::Key, vec![], "na"));
        let c = yaml("a: 1\nb:\n  c: 2\nd: |").unwrap();
        assert_eq!(c.path, vec![k("d")], "dedent closes the previous scopes");
    }

    #[test]
    fn yaml_sequences_including_same_indent_dash() {
        let c = yaml("steps:\n  - uses: x\n  - ru|").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone(), c.prefix.as_str()), (Slot::Key, vec![k("steps"), Seg::Index(0)], "ru"));
        let c = yaml("steps:\n- uses: x\n  with:\n    a: 1\n- |").unwrap();
        assert_eq!((c.slot.clone(), c.path.clone()), (Slot::Key, vec![k("steps"), Seg::Index(0)]), "dash at the parent's indent");
        let c = yaml("steps:\n  - name: a\n    |").unwrap();
        assert_eq!(c.path, vec![k("steps"), Seg::Index(0)]);
        assert!(c.existing.contains("name"));
        let c = yaml("containers:\n  - name: web\n    ports:\n      - containerPort: 80\n      - |").unwrap();
        assert_eq!(c.path, vec![k("containers"), Seg::Index(0), k("ports"), Seg::Index(0)]);
    }

    #[test]
    fn yaml_indent_and_colon_detection() {
        let c = yaml("a:\n  b|: 1").unwrap();
        assert!(c.colon_follows);
        assert_eq!(c.indent, 2);
        let c = yaml("a:\n  b|").unwrap();
        assert!(!c.colon_follows);
        // Comments and blank lines between do not break the path.
        let c = yaml("a:\n  # note\n\n  b:\n    |").unwrap();
        assert_eq!(c.path, vec![k("a"), k("b")]);
    }
}
