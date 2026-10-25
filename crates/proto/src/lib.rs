//! Protocol between the UI and the agent. The same messages are used in-process locally and
//! remotely over `ssh` stdio.
//!
//! Frame: 4-byte little-endian length + `Frame` encoded with postcard.
//!
//! Multiplexing: each request has an `id`; replies carry the same `id` (repeatedly for
//! streams like search). `id == 0` is special: agent → client it's an unsolicited
//! event (terminal output), client → agent it means "no reply wanted"
//! (terminal input, so a keystroke doesn't wait for a round trip).
#![deny(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Compared in the handshake; on mismatch the agent is reinstalled.
pub const VERSION: u32 = 9;

/// Max size of one frame. Larger files are read in chunks.
pub const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Largest file the agent returns in one go.
pub const MAX_FILE: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame<T> {
    pub id: u32,
    pub body: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    /// `session`: ID of the previous session when reconnecting (in daemon mode
    /// terminals and watches resume where they were).
    Hello {
        client_version: u32,
        session: Option<String>,
    },
    /// Client is leaving on purpose (window closed): the daemon ends the session without waiting `GRACE`; shells and processes stop at once.
    Bye,
    /// Liveness probe: the dispatch thread answers `Done` at once (independent of the pool and processes).
    Ping,
    ReadDir {
        path: String,
    },
    Stat {
        path: String,
    },
    Read {
        path: String,
    },
    /// `expect_hash`: hash of the version the user started editing.
    /// If the file on disk differs nothing is written and `Conflict` comes back. `None`: new file
    /// or deliberate overwrite.
    Write {
        path: String,
        data: Vec<u8>,
        expect_hash: Option<u64>,
    },
    /// Files under the root (relative paths), honouring .gitignore. For quick open.
    ListFiles {
        root: String,
        limit: u32,
    },
    /// New empty file; `AlreadyExists` if present (never overwrites).
    CreateFile {
        path: String,
    },
    CreateDir {
        path: String,
    },
    /// `AlreadyExists` if the target exists.
    Rename {
        from: String,
        to: String,
    },
    /// File or folder (with its contents). The root and the home directory are refused.
    Delete {
        path: String,
    },
    /// Search in files: `SearchChunk` parts, then `SearchDone`.
    Search(SearchQuery),
    /// Stop a running stream (search).
    Cancel {
        id: u32,
    },
    /// Open a shell. `shell`: the user's shell (`$SHELL`) if unset.
    PtySpawn {
        cols: u16,
        rows: u16,
        cwd: Option<String>,
        shell: Option<String>,
    },
    PtyInput {
        pty: u32,
        data: Vec<u8>,
    },
    PtyResize {
        pty: u32,
        cols: u16,
        rows: u16,
    },
    PtyKill {
        pty: u32,
    },
    /// Change the watched path set (open files, folders expanded in the tree).
    /// Changes arrive as `FsChanged` events.
    Watch {
        paths: Vec<String>,
    },
    /// Run the command, `Exited` when done (git). A bare program name is looked up with the login shell.
    Exec {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        stdin: Option<Vec<u8>>,
    },
    /// Long-running process over pipes (language server). Output as `ProcOutput` events.
    /// Processes are terminated if the connection drops; the client restarts them.
    ProcSpawn {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
    },
    ProcInput {
        proc: u32,
        data: Vec<u8>,
    },
    ProcKill {
        proc: u32,
    },
    /// Large file: line count and size (`LineIndex`); the index is cached in the agent.
    LineIndex {
        path: String,
    },
    /// Range of lines from a large file (`Lines`).
    ReadLines {
        path: String,
        start: u64,
        count: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub root: String,
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// Globs (gitignore syntax): everything if empty.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub max_results: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMatches {
    /// Relative to the root.
    pub path: String,
    pub matches: Vec<LineMatch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LineMatch {
    /// 1-based.
    pub line: u32,
    /// Preview of the line (long lines are truncated).
    pub preview: String,
    /// Matches within the preview (bytes).
    pub ranges: Vec<(u32, u32)>,
    /// Column of the first match in the line (characters, 0-based), to scroll to it.
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Hello(HostInfo),
    Entries(Vec<DirEntry>),
    Stat(Meta),
    File {
        data: Vec<u8>,
        hash: u64,
    },
    Written {
        hash: u64,
    },
    Files {
        paths: Vec<String>,
        truncated: bool,
    },
    Done,
    SearchChunk(Vec<FileMatches>),
    SearchDone {
        files: u32,
        matches: u32,
        truncated: bool,
        cancelled: bool,
    },
    PtySpawned {
        pty: u32,
    },
    /// Event (id 0).
    PtyOutput {
        pty: u32,
        data: Vec<u8>,
    },
    /// Event (id 0).
    PtyExit {
        pty: u32,
        code: Option<i32>,
    },
    /// Event (id 0): change in the watched paths.
    FsChanged(Vec<(String, ChangeKind)>),
    Error(Error),
    Exited {
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    ProcSpawned {
        proc: u32,
    },
    /// Event (id 0).
    ProcOutput {
        proc: u32,
        data: Vec<u8>,
    },
    /// Event (id 0).
    ProcExit {
        proc: u32,
        code: Option<i32>,
    },
    LineIndex {
        lines: u64,
        size: u64,
    },
    Lines(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    pub version: u32,
    pub os: String,
    pub arch: String,
    pub home: String,
    pub hostname: String,
    /// Reconnected to the previous session (terminals are alive).
    pub resumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: Kind,
    /// If a symlink, the type of the target (broken link: `None`).
    pub target_kind: Option<Kind>,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir || self.target_kind == Some(Kind::Dir)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    /// Unix epoch milliseconds.
    pub mtime_ms: i64,
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Error {
    NotFound(String),
    PermissionDenied(String),
    IsDirectory(String),
    AlreadyExists(String),
    TooLarge {
        path: String,
        size: u64,
        limit: u64,
    },
    /// The disk differs from the version the edit started from.
    Conflict {
        path: String,
        current_hash: u64,
    },
    VersionMismatch {
        agent: u32,
        client: u32,
    },
    InvalidPattern(String),
    Unsupported(String),
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound(p) => write!(f, "not found: {p}"),
            Error::PermissionDenied(p) => write!(f, "permission denied: {p}"),
            Error::IsDirectory(p) => write!(f, "is a folder: {p}"),
            Error::AlreadyExists(p) => write!(f, "already exists: {p}"),
            Error::TooLarge { path, size, limit } => {
                write!(f, "file too large ({} MB, limit {} MB): {path}", size >> 20, limit >> 20)
            }
            Error::Conflict { path, .. } => write!(f, "changed on disk: {path}"),
            Error::VersionMismatch { agent, client } => {
                write!(f, "protocol version mismatch (agent {agent}, client {client})")
            }
            Error::InvalidPattern(m) => write!(f, "invalid search pattern: {m}"),
            Error::Unsupported(m) => write!(f, "not supported here: {m}"),
            Error::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

/// Hash of the file contents: for conflict detection (not cryptographic).
pub fn content_hash(data: &[u8]) -> u64 {
    xxhash_rust::xxh3::xxh3_64(data)
}

pub fn write_frame<T: Serialize>(w: &mut impl Write, frame: &Frame<T>) -> io::Result<()> {
    // `InvalidData`: rejected before any byte was written, so the connection is intact (oversized reply or unserializable
    // body). Real write errors use other kinds.
    let bytes = postcard::to_stdvec(frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame too large: {} bytes", bytes.len())));
    }
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// `Ok(None)` if the stream closed cleanly.
pub fn read_frame<T: for<'de> Deserialize<'de>>(r: &mut impl Read) -> io::Result<Option<Frame<T>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::other(format!("frame too large: {len} bytes")));
    }
    // Grow with what actually arrives: a peer that announces a huge frame and stalls must not pin `len` bytes.
    let mut buf = Vec::new();
    r.take(len as u64).read_to_end(&mut buf)?;
    if buf.len() < len {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    postcard::from_bytes(&buf).map(Some).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let req = Frame { id: 7, body: Request::Write { path: "a/ğüş.rs".into(), data: vec![1, 2, 3], expect_hash: Some(9) } };
        let mut buf = Vec::new();
        write_frame(&mut buf, &req).unwrap();
        write_frame(&mut buf, &Frame { id: 8, body: Request::Hello { client_version: VERSION, session: None } }).unwrap();
        let mut r = buf.as_slice();
        let a: Frame<Request> = read_frame(&mut r).unwrap().unwrap();
        let b: Frame<Request> = read_frame(&mut r).unwrap().unwrap();
        assert_eq!((a.id, a.body), (7, req.body));
        assert_eq!(b.id, 8);
        assert!(read_frame::<Request>(&mut r).unwrap().is_none());
    }

    #[test]
    fn truncated_body_is_an_error_not_a_clean_close() {
        let mut bytes = (MAX_FRAME as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&[1, 2, 3]);
        let err = read_frame::<Request>(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn announced_length_is_not_preallocated() {
        struct Stalled(usize);
        impl Read for Stalled {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.0 < 4 {
                    buf[..1].copy_from_slice(&(MAX_FRAME as u32).to_le_bytes()[self.0..=self.0]);
                    self.0 += 1;
                    return Ok(1);
                }
                Err(io::ErrorKind::TimedOut.into())
            }
        }
        // Would be a 64 MiB zeroed allocation before the first body byte arrives; here it fails fast on the stalled read.
        assert_eq!(read_frame::<Request>(&mut Stalled(0)).unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn garbage_body_is_an_error() {
        let mut bytes = 5u32.to_le_bytes().to_vec();
        bytes.extend_from_slice(&[0xff; 5]);
        assert!(read_frame::<Request>(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn rejects_oversized_frame_header() {
        let mut r: &[u8] = &(u32::MAX).to_le_bytes();
        assert!(read_frame::<Request>(&mut r).is_err());
    }
}
