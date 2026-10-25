//! Merge conflict markers: find, resolve.
//!
//! ```text
//! <<<<<<< HEAD          ← start
//! ours                  ← current
//! ||||||| base          ← base (diff3/zdiff3; optional)
//! common ancestor
//! =======               ← sep
//! theirs                ← incoming
//! >>>>>>> branch        ← end
//! ```
//! Marker: exactly 7 characters at the line start, then a space or end of line.
//! Unclosed or out-of-order blocks are ignored (so text isn't deleted by accident).

use buffer::Rope;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Line numbers (0-based).
    pub start: usize,
    pub base: Option<usize>,
    pub sep: usize,
    pub end: usize,
    /// Names after `<<<<<<<` and `>>>>>>>` ("HEAD", branch name).
    pub current_label: String,
    pub incoming_label: String,
}

impl Conflict {
    pub fn current_lines(&self) -> std::ops::Range<usize> {
        self.start + 1..self.base.unwrap_or(self.sep)
    }

    pub fn incoming_lines(&self) -> std::ops::Range<usize> {
        self.sep + 1..self.end
    }

    /// Common ancestor lines (only with diff3/zdiff3 markers).
    pub fn base_lines(&self) -> Option<std::ops::Range<usize>> {
        self.base.map(|b| b + 1..self.sep)
    }

    pub fn contains_line(&self, line: usize) -> bool {
        (self.start..=self.end).contains(&line)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Current,
    Incoming,
    Both,
    /// Incoming first, then current.
    BothIncomingFirst,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Start,
    Base,
    Sep,
    End,
}

/// Marker and label of a line. `=======` takes no label (`======= x` isn't a marker).
fn marker(line: &str) -> Option<(Marker, &str)> {
    let line = line.trim_end_matches(['\n', '\r']);
    let kind = match line.as_bytes().first()? {
        b'<' => Marker::Start,
        b'|' => Marker::Base,
        b'=' => Marker::Sep,
        b'>' => Marker::End,
        _ => return None,
    };
    let c = line.as_bytes()[0];
    if line.len() < 7 || !line.as_bytes()[..7].iter().all(|&b| b == c) {
        return None;
    }
    let rest = &line[7..];
    match (kind, rest.chars().next()) {
        (Marker::Sep, None) => Some((kind, "")),
        (Marker::Sep, _) => None,
        (_, None) => Some((kind, "")),
        (_, Some(' ')) => Some((kind, rest[1..].trim())),
        _ => None,
    }
}

/// Conflicts from a list of lines.
pub fn find_in_lines<'a>(lines: impl Iterator<Item = (usize, &'a str)>) -> Vec<Conflict> {
    let mut out = Vec::new();
    let mut open: Option<Conflict> = None;
    for (i, line) in lines {
        let Some((kind, label)) = marker(line) else { continue };
        match (kind, open.as_mut()) {
            // If a new block starts while one is unclosed, the old one is broken: continue with the new one.
            (Marker::Start, _) => {
                open =
                    Some(Conflict { start: i, base: None, sep: usize::MAX, end: usize::MAX, current_label: label.to_string(), incoming_label: String::new() })
            }
            (Marker::Base, Some(c)) if c.base.is_none() && c.sep == usize::MAX => c.base = Some(i),
            (Marker::Sep, Some(c)) if c.sep == usize::MAX => c.sep = i,
            (Marker::End, Some(c)) if c.sep != usize::MAX => {
                c.end = i;
                c.incoming_label = label.to_string();
                out.extend(open.take());
            }
            _ => {}
        }
    }
    out
}

/// Is there a marker in the rope: cheap pre-check before scanning line by line. The end of the previous
/// chunk is added too so a marker straddling a chunk boundary isn't missed.
pub fn may_have_conflicts(rope: &Rope) -> bool {
    let finder = memchr::memmem::Finder::new("<<<<<<<");
    let mut prev: &str = "";
    for chunk in rope.chunks() {
        if finder.find(chunk.as_bytes()).is_some() {
            return true;
        }
        // Marker on the boundary: only join and look if both sides have `<` (rare).
        if prev.ends_with('<') && chunk.starts_with('<') {
            let tail = &prev[prev.len() - prev.bytes().rev().take_while(|&b| b == b'<').count().min(6)..];
            let head = &chunk[..chunk.bytes().take_while(|&b| b == b'<').count().min(6)];
            if tail.len() + head.len() >= 7 {
                return true;
            }
        }
        prev = chunk;
    }
    false
}

/// Conflicts in the rope. With no markers (almost always) no line is looked at.
pub fn find(rope: &Rope) -> Vec<Conflict> {
    if !may_have_conflicts(rope) {
        return Vec::new();
    }
    // Sequential walk; only lines starting with 7 identical marker characters are converted to text.
    let owned: Vec<(usize, String)> = rope
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let mut chars = line.chars();
            let first = chars.next();
            matches!(first, Some('<' | '|' | '=' | '>')) && chars.take(6).filter(|&ch| Some(ch) == first).count() == 6
        })
        .map(|(i, line)| (i, line.to_string()))
        .collect();
    find_in_lines(owned.iter().map(|(i, s)| (*i, s.as_str())))
}

/// Resolution: text to replace the lines `start..=end` (with line endings).
pub fn resolution(rope: &Rope, c: &Conflict, choice: Choice) -> String {
    let take = |r: std::ops::Range<usize>| -> String { r.map(|l| rope.line(l).to_string()).collect() };
    match choice {
        Choice::Current => take(c.current_lines()),
        Choice::Incoming => take(c.incoming_lines()),
        Choice::Both | Choice::BothIncomingFirst => {
            let (a, b) = (take(c.current_lines()), take(c.incoming_lines()));
            let (a, b) = if choice == Choice::Both { (a, b) } else { (b, a) };
            // If the first section ends without a newline (not EOF; can't happen, but) separate when joining.
            if a.is_empty() || a.ends_with('\n') { a + &b } else { format!("{a}\n{b}") }
        }
    }
}

/// Character range of the block (from the start of the `<<<<<<<` line to the end of the `>>>>>>>` line, line ending included).
pub fn char_range(rope: &Rope, c: &Conflict) -> std::ops::Range<usize> {
    let end_line = c.end + 1;
    let end = if end_line >= rope.len_lines() { rope.len_chars() } else { rope.line_to_char(end_line) };
    rope.line_to_char(c.start)..end
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "a\n<<<<<<< HEAD\nbizim\n=======\nonların\n>>>>>>> özellik/x\nb\n<<<<<<< HEAD\n=======\nyeni\n>>>>>>> main\n";

    #[test]
    fn finds_blocks_with_labels() {
        let r = Rope::from_str(TWO);
        let cs = find(&r);
        assert_eq!(cs.len(), 2);
        assert_eq!((cs[0].start, cs[0].sep, cs[0].end), (1, 3, 5));
        assert_eq!((cs[0].current_label.as_str(), cs[0].incoming_label.as_str()), ("HEAD", "özellik/x"));
        assert_eq!(cs[1].current_lines(), 8..8, "empty current section");
    }

    #[test]
    fn resolves_each_way() {
        let r = Rope::from_str(TWO);
        let c = &find(&r)[0];
        assert_eq!(resolution(&r, c, Choice::Current), "bizim\n");
        assert_eq!(resolution(&r, c, Choice::Incoming), "onların\n");
        assert_eq!(resolution(&r, c, Choice::Both), "bizim\nonların\n");
        let range = char_range(&r, c);
        let mut s = r.clone();
        s.remove(range.clone());
        s.insert(range.start, &resolution(&r, c, Choice::Both));
        assert_eq!(s.to_string(), "a\nbizim\nonların\nb\n<<<<<<< HEAD\n=======\nyeni\n>>>>>>> main\n");
    }

    #[test]
    fn diff3_base_is_dropped_from_current() {
        let r = Rope::from_str("<<<<<<< ours\nx\n||||||| base\neski\n=======\ny\n>>>>>>> theirs\n");
        let c = &find(&r)[0];
        assert_eq!(c.base, Some(2));
        assert_eq!(resolution(&r, c, Choice::Current), "x\n");
        assert_eq!(resolution(&r, c, Choice::Both), "x\ny\n");
    }

    #[test]
    fn look_alikes_and_broken_blocks_are_ignored() {
        // 8 characters, labeled separator, indented marker, unclosed block, block without separator.
        let text = "<<<<<<<< not\n======= x\n  <<<<<<< girintili\n<<<<<<< açık\nx\n=======\ny\n\n<<<<<<< a\nb\n>>>>>>> c\n";
        assert!(find(&Rope::from_str(text)).is_empty(), "{:?}", find(&Rope::from_str(text)));
        // A well-formed block after an unclosed one is found.
        let text = "<<<<<<< a\nx\n<<<<<<< HEAD\n1\n=======\n2\n>>>>>>> b\n";
        let cs = find(&Rope::from_str(text));
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].start, 2);
    }

    #[test]
    fn crlf_and_last_line_without_newline() {
        let r = Rope::from_str("<<<<<<< HEAD\r\nx\r\n=======\r\ny\r\n>>>>>>> b");
        let c = &find(&r)[0];
        assert_eq!(resolution(&r, c, Choice::Incoming), "y\r\n");
        assert_eq!(char_range(&r, c), 0..r.len_chars());
    }

    #[test]
    fn marker_split_across_rope_chunks_is_seen() {
        // Rope chunks are ~1 KB: shift so the marker lands exactly on the boundary.
        for pad in 0..1200 {
            let text = format!("{}\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\n", "a".repeat(pad));
            assert_eq!(find(&Rope::from_str(&text)).len(), 1, "pad {pad}");
        }
        assert!(!may_have_conflicts(&Rope::from_str(&"x\n".repeat(50_000))));
    }

    /// Runs on every keystroke: must be fast on a 50k-line (~2 MB) file without markers; line scanning
    /// should stay reasonable on a file with markers too.
    #[test]
    fn scanning_a_large_file_is_cheap() {
        let line = "    let değişken = fonksiyon(argüman, 42); // yorum satırı\n";
        let clean = Rope::from_str(&line.repeat(50_000));
        let mut marked = line.repeat(50_000);
        marked.push_str("<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\n");
        let marked = Rope::from_str(&marked);
        let time = |r: &Rope| {
            let t = std::time::Instant::now();
            for _ in 0..10 {
                std::hint::black_box(find(r));
            }
            t.elapsed() / 10
        };
        let (a, b) = (time(&clean), time(&marked));
        eprintln!("conflict scan, 50k lines: unmarked {a:?}, marked {b:?}");
        assert_eq!(find(&marked).len(), 1);
        if !cfg!(debug_assertions) {
            assert!(a < std::time::Duration::from_micros(800), "unmarked {a:?}");
            assert!(b < std::time::Duration::from_millis(8), "marked {b:?}");
        }
    }
}
