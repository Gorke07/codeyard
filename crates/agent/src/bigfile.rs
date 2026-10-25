//! Large files: one streaming scan samples a byte offset every `STEP` lines
//! (2 GB / 20 million lines ≈ 40 KB of index). Lines are read from the nearest
//! sample; the file is never loaded whole.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

pub const STEP: u64 = 4096;
/// Upper limit on a displayed line (so a single-line giant JSON doesn't blow up memory).
const MAX_LINE: usize = 10_000;

#[derive(Debug, Clone)]
pub struct Index {
    pub size: u64,
    mtime: Option<SystemTime>,
    pub lines: u64,
    /// Number of `\n` (`lines` is one more than this); to resume scanning where it left off on a growing file.
    newlines: u64,
    /// `samples[i]`: the byte at which line `i * STEP` starts.
    samples: Vec<u64>,
    /// Fingerprint of the scanned tail: don't continue incrementally if the file was rotated and grew again.
    tail: u64,
}

/// Number of bytes read from the scanned tail for the fingerprint.
const FINGERPRINT: u64 = 256;

fn fingerprint(f: &mut File, size: u64) -> io::Result<u64> {
    let from = size.saturating_sub(FINGERPRINT);
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; (size - from) as usize];
    f.read_exact(&mut buf)?;
    // FNV-1a
    Ok(buf.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3)))
}

impl Index {
    /// Valid if the file hasn't changed.
    pub fn fresh(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|m| m.len() == self.size && m.modified().ok() == self.mtime)
    }

    /// If the file only grew by appending (a log), scan just the new part and extend the index.
    /// `None` if it shrank or the previous tail changed (rotated log): it must be rebuilt from scratch.
    pub fn extend(&self, path: &Path) -> io::Result<Option<Index>> {
        let meta = std::fs::metadata(path)?;
        if meta.len() < self.size {
            return Ok(None);
        }
        let mut f = File::open(path)?;
        if self.size > 0 && fingerprint(&mut f, self.size)? != self.tail {
            return Ok(None);
        }
        f.seek(SeekFrom::Start(self.size))?;
        let mut samples = self.samples.clone();
        let (pos, newlines) = scan(&mut f, self.size, self.newlines, &mut samples)?;
        let tail = if pos > 0 { fingerprint(&mut f, pos)? } else { 0 };
        Ok(Some(Index { size: pos, mtime: meta.modified().ok(), lines: newlines + 1, newlines, samples, tail }))
    }
}

/// Scan `f` (currently at `pos`) to the end; the new (byte offset, `\n` count) and samples.
fn scan(f: &mut File, mut pos: u64, mut newlines: u64, samples: &mut Vec<u64>) -> io::Result<(u64, u64)> {
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for (i, &b) in buf[..n].iter().enumerate() {
            if b == b'\n' {
                newlines += 1;
                if newlines.is_multiple_of(STEP) {
                    samples.push(pos + i as u64 + 1);
                }
            }
        }
        pos += n as u64;
    }
    Ok((pos, newlines))
}

pub fn build(path: &Path) -> io::Result<Index> {
    let meta = std::fs::metadata(path)?;
    // FIFOs, devices and sockets read "forever": only regular files are scanned.
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let mut f = File::open(path)?;
    let mut samples = vec![0u64];
    let (pos, newlines) = scan(&mut f, 0, 0, &mut samples)?;
    let tail = if pos > 0 { fingerprint(&mut f, pos)? } else { 0 };
    // As in the editor: one more than the number of line ends (including the trailing empty line). Size: the bytes actually scanned
    // (so the later `fresh`/`extend` compare correctly for a file that grows during the scan).
    Ok(Index { size: pos, mtime: meta.modified().ok(), lines: newlines + 1, newlines, samples, tail })
}

/// Read to the end of the line and discard, without allocating (so a single line of hundreds of MB doesn't bloat memory). `true` if a line end was found,
/// `false` if the file ended (with no bytes read at all).
fn skip_line(f: &mut impl BufRead) -> io::Result<bool> {
    let mut read_any = false;
    loop {
        let chunk = f.fill_buf()?;
        if chunk.is_empty() {
            return Ok(read_any);
        }
        read_any = true;
        match chunk.iter().position(|&b| b == b'\n') {
            Some(i) => {
                f.consume(i + 1);
                return Ok(true);
            }
            None => {
                let n = chunk.len();
                f.consume(n);
            }
        }
    }
}

/// Up to `count` lines from `start` (lossily converted if not UTF-8).
pub fn read_lines(path: &Path, index: &Index, start: u64, count: u32) -> io::Result<Vec<String>> {
    // Past the indexed lines there is no sample to start from; falling back to offset 0 would return the file's first
    // lines labelled as `start`.
    let Some(&offset) = index.samples.get((start / STEP) as usize) else { return Ok(Vec::new()) };
    let mut f = BufReader::with_capacity(1 << 16, File::open(path)?);
    let sample = (start / STEP) as usize;
    f.seek(SeekFrom::Start(offset))?;
    let mut line = sample as u64 * STEP;
    let mut buf = Vec::new();
    // Skip from the sample to the requested line.
    while line < start {
        if !skip_line(&mut f)? {
            return Ok(Vec::new());
        }
        line += 1;
    }
    let mut out = Vec::with_capacity(count as usize);
    while out.len() < count as usize {
        buf.clear();
        // Read the long line up to the limit, skip the rest.
        let n = (&mut f).take(MAX_LINE as u64 * 4).read_until(b'\n', &mut buf)?;
        if n == 0 {
            break;
        }
        if buf.last() != Some(&b'\n') && n == MAX_LINE * 4 {
            skip_line(&mut f)?;
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        let mut s = String::from_utf8_lossy(&buf).into_owned();
        if s.chars().count() > MAX_LINE {
            s = s.chars().take(MAX_LINE).collect::<String>() + " …";
        }
        out.push(s);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest single allocation made by the current thread: the proof that skipping a giant line does not buffer it.
    struct Biggest;
    thread_local! { static BIGGEST: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    // SAFETY: forwards every call unchanged to the system allocator; the bookkeeping touches only a const-initialised thread-local.
    unsafe impl std::alloc::GlobalAlloc for Biggest {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            let _ = BIGGEST.try_with(|b| b.set(b.get().max(layout.size())));
            unsafe { std::alloc::System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
            let _ = BIGGEST.try_with(|b| b.set(b.get().max(new_size)));
            unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
        }
    }
    #[global_allocator]
    static ALLOC: Biggest = Biggest;

    #[test]
    fn skipping_a_line_does_not_allocate_it() {
        let mut data = vec![b'a'; 3_000_000];
        data.extend_from_slice(b"\nsecond\nlast");
        let mut r = std::io::BufReader::with_capacity(4096, std::io::Cursor::new(data));
        BIGGEST.with(|b| b.set(0));
        assert!(skip_line(&mut r).unwrap());
        let biggest = BIGGEST.with(|b| b.get());
        assert!(biggest < 64 * 1024, "skipping a 3 MB line allocated {biggest} bytes in one piece");
        assert!(skip_line(&mut r).unwrap());
        assert!(skip_line(&mut r).unwrap(), "a final line without newline still counts");
        assert!(!skip_line(&mut r).unwrap(), "end of file");
    }

    /// A line over the limit is cut (with " …") and the rest of it is skipped: the next line must start where the file's next line starts,
    /// also when the byte limit falls inside a multi-byte character.
    #[test]
    fn long_lines_are_cut_and_the_next_line_stays_intact() {
        let dir = std::env::temp_dir().join(format!("codeyard-big-long-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("long.txt");
        let mut text = String::new();
        text.push_str(&"b".repeat(MAX_LINE));
        text.push('\n');
        text.push_str(&"a".repeat(MAX_LINE + 500));
        text.push('\n');
        text.push_str(&"x".repeat(MAX_LINE * 4 * 3));
        text.push('\n');
        text.push_str(&"€".repeat(MAX_LINE * 3));
        text.push_str("\r\nsecond\nlast");
        std::fs::write(&p, &text).unwrap();
        let idx = build(&p).unwrap();
        let got = read_lines(&p, &idx, 0, 10).unwrap();
        let cut = |c: &str| format!("{} …", c.repeat(MAX_LINE));
        assert_eq!(got.len(), 6, "{:?}", got.iter().map(|l| l.chars().count()).collect::<Vec<_>>());
        // Not assert_eq!: a failure would print the 10 000-character lines.
        for (i, want) in [("b".repeat(MAX_LINE)), cut("a"), cut("x"), cut("€")].iter().enumerate() {
            assert!(got[i] == *want, "line {i}: {} chars, expected {} (the cut text or the whole line)", got[i].chars().count(), want.chars().count());
        }
        assert_eq!(&got[4..], ["second", "last"]);
        assert_eq!(read_lines(&p, &idx, 4, 1).unwrap(), vec!["second"], "skipping to a line behind a giant one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_and_read_across_samples() {
        let dir = std::env::temp_dir().join(format!("codeyard-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("log.txt");
        let text: String = (0..10_000).map(|i| format!("satır {i} ğüş\r\n")).collect();
        std::fs::write(&p, &text).unwrap();
        let idx = build(&p).unwrap();
        assert_eq!(idx.lines, 10_001, "empty line after the last line end");
        assert_eq!(idx.samples.len(), 3, "0, 4096, 8192");
        assert_eq!(read_lines(&p, &idx, 0, 2).unwrap(), vec!["satır 0 ğüş", "satır 1 ğüş"]);
        assert_eq!(read_lines(&p, &idx, 8191, 3).unwrap(), vec!["satır 8191 ğüş", "satır 8192 ğüş", "satır 8193 ğüş"]);
        assert_eq!(read_lines(&p, &idx, 9_999, 5).unwrap(), vec!["satır 9999 ğüş"]);
        assert!(read_lines(&p, &idx, 3 * STEP, 5).unwrap().is_empty(), "no sample past the index");
        assert!(read_lines(&p, &idx, u64::MAX, 5).unwrap().is_empty());
        assert!(idx.fresh(&p));
        std::fs::write(&p, "değişti").unwrap();
        assert!(!idx.fresh(&p));
        let idx = build(&p).unwrap();
        assert_eq!((idx.lines, read_lines(&p, &idx, 0, 9).unwrap()), (1, vec!["değişti".to_string()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appended_log_extends_without_rescanning_and_rotation_rebuilds() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("codeyard-big-ext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("app.log");
        let line = |i: u32| format!("2026-01-01 INFO line {i}\n");
        std::fs::write(&p, (0..5000).map(line).collect::<String>()).unwrap();
        let idx = build(&p).unwrap();
        assert_eq!(idx.lines, 5001);
        // Append: 3000 more lines (crosses a sample boundary).
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all((5000..8000).map(line).collect::<String>().as_bytes()).unwrap();
        drop(f);
        let ext = idx.extend(&p).unwrap().expect("grew: incremental");
        let full = build(&p).unwrap();
        assert_eq!((ext.lines, ext.size, ext.samples.clone()), (full.lines, full.size, full.samples.clone()));
        assert_eq!(read_lines(&p, &ext, 7999, 1).unwrap(), vec![line(7999).trim_end().to_string()]);
        // Unchanged: same index.
        assert_eq!(ext.extend(&p).unwrap().unwrap().lines, ext.lines);
        // Rotated (shortened): from scratch.
        std::fs::write(&p, "yeni\n").unwrap();
        assert!(ext.extend(&p).unwrap().is_none());
        // Rotated and grew larger than before: the fingerprint doesn't match.
        std::fs::write(&p, (0..20_000).map(|i| format!("başka içerik {i}\n")).collect::<String>()).unwrap();
        assert!(ext.extend(&p).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
