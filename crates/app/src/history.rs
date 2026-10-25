//! Local history (like IntelliJ's "Local History"): a copy of the file is kept on every save,
//! so changes that never reached git can be restored too. Copies live in the data folder as
//! `history/<file digest>/<time ms>.txt`; at most `KEEP` per file, at most 30 days.

use std::path::{Path, PathBuf};

/// Number of copies kept per file.
pub const KEEP: usize = 40;
/// Files larger than this aren't copied.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_AGE_MS: u64 = 30 * 24 * 3600 * 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Save time (Unix ms).
    pub at_ms: u64,
    pub path: PathBuf,
}

/// FNV-1a: a digest of the path that is stable and safe as a file name.
fn digest(key: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// A file's copy folder; `host`: SSH alias (empty for local).
pub fn dir_for(data: &Path, host: &str, path: &str) -> PathBuf {
    data.join("history").join(digest(&format!("{host}\u{1f}{path}")))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// The copies, newest first.
pub fn list(dir: &Path) -> Vec<Entry> {
    let mut v: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let at_ms = name.strip_suffix(".txt")?.parse().ok()?;
            Some(Entry { at_ms, path: e.path() })
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.at_ms));
    v
}

/// Write a new copy. `false` if it equals the newest or is too large. Prunes old ones.
pub fn record(dir: &Path, text: &str) -> std::io::Result<bool> {
    if text.len() > MAX_BYTES {
        return Ok(false);
    }
    let existing = list(dir);
    if existing.first().is_some_and(|last| std::fs::read_to_string(&last.path).is_ok_and(|t| t == text)) {
        return Ok(false);
    }
    std::fs::create_dir_all(dir)?;
    let mut at = now_ms();
    // Second save in the same millisecond: keep the order intact.
    if let Some(last) = existing.first() {
        at = at.max(last.at_ms + 1);
    }
    std::fs::write(dir.join(format!("{at}.txt")), text)?;
    let now = now_ms();
    for (i, e) in existing.iter().enumerate() {
        // The new copy + (KEEP-1) old ones remain.
        if i + 1 >= KEEP || now.saturating_sub(e.at_ms) > MAX_AGE_MS {
            let _ = std::fs::remove_file(&e.path);
        }
    }
    Ok(true)
}

pub fn read(entry: &Entry) -> std::io::Result<String> {
    std::fs::read_to_string(&entry.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("codeyard-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn same_path_same_dir_and_hosts_are_separate() {
        let d = Path::new("/data");
        assert_eq!(dir_for(d, "", "/a/b.rs"), dir_for(d, "", "/a/b.rs"));
        assert_ne!(dir_for(d, "", "/a/b.rs"), dir_for(d, "srv", "/a/b.rs"));
        assert_ne!(dir_for(d, "", "/a/b.rs"), dir_for(d, "", "/a/c.rs"));
    }

    #[test]
    fn copies_are_recorded_newest_first_and_identical_text_is_skipped() {
        let dir = tmp("record");
        assert!(record(&dir, "bir").unwrap());
        assert!(!record(&dir, "bir").unwrap(), "identical content isn't written again");
        assert!(record(&dir, "iki").unwrap());
        assert!(record(&dir, "üç").unwrap());
        let l = list(&dir);
        assert_eq!(l.len(), 3);
        assert_eq!(read(&l[0]).unwrap(), "üç");
        assert_eq!(read(&l[2]).unwrap(), "bir");
        assert!(l[0].at_ms > l[1].at_ms && l[1].at_ms > l[2].at_ms);
    }

    #[test]
    fn old_copies_are_pruned_and_big_files_skipped() {
        let dir = tmp("prune");
        for i in 0..(KEEP + 5) {
            record(&dir, &format!("sürüm {i}")).unwrap();
        }
        let l = list(&dir);
        assert_eq!(l.len(), KEEP);
        assert_eq!(read(&l[0]).unwrap(), format!("sürüm {}", KEEP + 4), "the newest stays");
        assert!(!record(&dir, &"x".repeat(MAX_BYTES + 1)).unwrap());
        assert!(list(&tmp("yok")).is_empty());
    }
}
