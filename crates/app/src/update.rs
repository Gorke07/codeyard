//! In-app updater, the part without a UI: find out whether a newer release exists, download it, and prove that it is ours.
//! The installing half (swap the app bundle, run the Windows installer, relaunch) is in `update_install.rs`.
//!
//! What the network sees: `check` fetches two small files from `<repository>/releases/latest/download/`
//! (`SHA256SUMS.txt` and its Ed25519 signature) and, only when a newer version is listed, `RELEASE_NOTES.md`.
//! No GitHub API, no token, no rate limit. Nothing is requested unless the caller asks (the UI does so on the
//! `update.check` policy, see `Policy`).
//!
//! Trust chain, in this order: signature over the exact bytes of `SHA256SUMS.txt` (checked BEFORE the file is parsed)
//! -> hash of `RELEASE_NOTES.md` and of the installer from the signed list -> only then is anything opened or run.
//! The version number is taken from the signed asset names, never from the transport, and is only offered when it is
//! strictly newer than the running one.
//!
//! Use from a background thread (everything here blocks):
//! ```text
//! let Some(release) = update::check(env!("CARGO_PKG_VERSION"))? else { return up_to_date };
//! let dl = update::download(&release, &mut |p| show(p.done, p.total), &cancel)?;
//! let plan = update::stage_and_install(&dl, &update::install_kind())?;
//! if let Some(mut c) = update::relaunch_command(&plan) { c.spawn()?; } // then quit the app
//! ```

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use ring::signature::{ED25519, UnparsedPublicKey};
use sha2::{Digest, Sha256};

/// Public keys whose signature on `SHA256SUMS.txt` is accepted (raw Ed25519, 32 bytes, hex).
///
/// Key rotation: (1) add the new public key here and ship that release, still signed with the old key;
/// (2) once most users run it, sign releases with the new key only; (3) a later release drops the old key.
/// A user who skips step 1 has to reinstall by hand, because the new key signs files the old build rejects.
/// The matching private key is never in the repository (see docs/releasing.md).
const TRUSTED_KEYS: &[[u8; 32]] = &[key("47bc1afc0c2c70a4a72ffd0b5ba535397bd9ca83551dfc30a12d64d7cf96d67c")];

const fn key(hex: &str) -> [u8; 32] {
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("key must be lowercase hex"),
        }
    }
    let h = hex.as_bytes();
    assert!(h.len() == 64, "key must be 64 hex digits");
    let mut out = [0; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = nibble(h[2 * i]) << 4 | nibble(h[2 * i + 1]);
        i += 1;
    }
    out
}

const SUMS: &str = "SHA256SUMS.txt";
const SIG: &str = "SHA256SUMS.txt.sig";
const NOTES: &str = "RELEASE_NOTES.md";
const SIG_LEN: usize = 64;
const SUMS_MAX: usize = 64 << 10;
const NOTES_MAX: usize = 256 << 10;
const INSTALLER_MAX: u64 = 400 << 20;
const SMALL_TIMEOUT_SECS: &str = "30";
const INSTALLER_TIMEOUT_SECS: &str = "1800";
const POLL: Duration = Duration::from_millis(100);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// File name suffix of the installer this build downloads (`Codeyard-<version>-<suffix>`); `None` where we ship none.
const PLATFORM_SUFFIX: Option<&str> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("macos-arm64.dmg")
} else if cfg!(all(windows, target_arch = "x86_64")) {
    Some("windows-x64-setup.exe")
} else {
    None
};

/// Every asset name that carries the version, used to read the version out of the signed list.
const VERSIONED_SUFFIXES: [&str; 4] = ["macos-arm64.dmg", "macos-arm64.tar.gz", "windows-x64-setup.exe", "windows-x64.zip"];

// --- Public types ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Lower-case hex, from the signed `SHA256SUMS.txt`.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// `major.minor.patch`, digits only (no pre-release tags).
    pub version: String,
    /// `RELEASE_NOTES.md`, hash-checked, valid UTF-8 (Markdown).
    pub notes: String,
    /// The installer for this platform; `None` where there is none (Linux, Intel Macs): offer the release page instead.
    pub asset: Option<Asset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    /// From the server's `Content-Length`; `None` until it is known (or when the server does not send one).
    pub total: Option<u64>,
}

/// A hash-verified installer in a private folder under the state dir. The folder is wiped by the next `download`.
#[derive(Debug)]
pub struct Downloaded {
    pub release: Release,
    pub(crate) path: PathBuf,
}

/// Setting `update.check`. Only `Daily` makes the app contact the network on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Never in the background; the UI asks the user (once) whether to turn on daily checks.
    Ask,
    Daily,
    /// Only when the user runs the command.
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPolicy;

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Policy::Ask => "ask",
            Policy::Daily => "daily",
            Policy::Manual => "manual",
        }
    }
}

impl FromStr for Policy {
    type Err = UnknownPolicy;
    fn from_str(s: &str) -> Result<Self, UnknownPolicy> {
        [Policy::Ask, Policy::Daily, Policy::Manual].into_iter().find(|p| p.as_str() == s).ok_or(UnknownPolicy)
    }
}

impl fmt::Display for UnknownPolicy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("expected \"ask\", \"daily\" or \"manual\"")
    }
}

/// Whether the app should look for an update now. Only `Daily` ever says yes: after 24 hours, when there was no
/// check yet, or when the last check lies in the future (clock set back; checking again also repairs the stored time).
pub fn should_check_now(last_check: Option<SystemTime>, now: SystemTime, policy: Policy) -> bool {
    policy == Policy::Daily && last_check.is_none_or(|t| now.duration_since(t).map_or(true, |age| age >= DAY))
}

// --- Errors ---

/// What the UI can do about an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Offline, server trouble, a timeout: offer "try again".
    Network,
    /// The files did not pass verification. Nothing was installed; do not retry blindly, point to the release page.
    Untrusted,
    /// Verified, but this installation cannot be updated automatically: point to the release page.
    Install,
    Cancelled,
}

#[derive(Debug)]
pub enum UpdateError {
    Network(String),
    /// A helper program (curl, hdiutil, ditto, plutil, xattr, the installer) could not be run or failed.
    Tool {
        tool: &'static str,
        detail: String,
    },
    BadSignature,
    BadSums(String),
    BadVersion(String),
    /// The signed list does not mention a file we need.
    MissingEntry(String),
    NotText(String),
    HashMismatch(String),
    TooLarge {
        file: String,
        limit: u64,
    },
    NotNewer {
        offered: String,
        current: String,
    },
    NoInstaller,
    /// `InstallKind::Manual`: nothing to install automatically.
    ManualOnly,
    NoWritePermission(PathBuf),
    BundleMismatch(String),
    CodesignFailed(String),
    MountFailed(String),
    /// The swap failed and the old app could not be moved back: `backup` holds the previous version.
    RollbackFailed {
        app: PathBuf,
        backup: PathBuf,
        cause: io::Error,
    },
    Io {
        what: &'static str,
        source: io::Error,
    },
    Cancelled,
}

impl UpdateError {
    pub fn kind(&self) -> ErrorKind {
        use UpdateError::*;
        match self {
            Network(_) => ErrorKind::Network,
            BadSignature | BadSums(_) | BadVersion(_) | MissingEntry(_) | NotText(_) | HashMismatch(_) | TooLarge { .. } | NotNewer { .. } => {
                ErrorKind::Untrusted
            }
            Tool { .. }
            | NoInstaller
            | ManualOnly
            | NoWritePermission(_)
            | BundleMismatch(_)
            | CodesignFailed(_)
            | MountFailed(_)
            | RollbackFailed { .. }
            | Io { .. } => ErrorKind::Install,
            Cancelled => ErrorKind::Cancelled,
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use UpdateError::*;
        match self {
            Network(e) => write!(f, "Could not reach the update server: {e}"),
            Tool { tool, detail } => write!(f, "The helper program “{tool}” failed: {detail}"),
            BadSignature => f.write_str("The list of release files is not signed by Codeyard. Nothing was downloaded or installed."),
            BadSums(e) => write!(f, "The list of release files is malformed: {e}"),
            BadVersion(v) => write!(f, "The release has an unusable version number: {v}"),
            MissingEntry(n) => write!(f, "The signed release list does not contain {n}."),
            NotText(n) => write!(f, "{n} is not valid text."),
            HashMismatch(n) => write!(f, "{n} does not match its signed checksum, so it was discarded."),
            TooLarge { file, limit } => write!(f, "{file} is larger than the allowed {} MB and was refused.", limit.div_ceil(1 << 20)),
            NotNewer { offered, current } => write!(f, "Version {offered} is not newer than the running version {current}."),
            NoInstaller => f.write_str("There is no installer for this system in the release."),
            ManualOnly => f.write_str("This copy of Codeyard cannot update itself; download the new version from the release page."),
            NoWritePermission(d) => write!(f, "Codeyard cannot write to {}, so it cannot replace itself there.", d.display()),
            BundleMismatch(e) => write!(f, "The downloaded app is not the expected one: {e}"),
            CodesignFailed(e) => write!(f, "The downloaded app failed the code signature check: {e}"),
            MountFailed(e) => write!(f, "The downloaded disk image could not be opened: {e}"),
            RollbackFailed { app, backup, cause } => {
                write!(
                    f,
                    "The update failed and the previous version could not be restored ({cause}). It is saved at {}; move it back to {}.",
                    backup.display(),
                    app.display()
                )
            }
            Io { what, source } => write!(f, "Could not {what}: {source}"),
            Cancelled => f.write_str("The update was cancelled."),
        }
    }
}

impl std::error::Error for UpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            UpdateError::Io { source, .. } | UpdateError::RollbackFailed { cause: source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn io_err(what: &'static str) -> impl FnOnce(io::Error) -> UpdateError {
    move |source| UpdateError::Io { what, source }
}

// --- Configuration ---

/// Where to fetch from and whom to trust. Built from the environment in the public functions; tests build their own.
pub(crate) struct Config {
    base: String,
    keys: Vec<[u8; 32]>,
    suffix: Option<&'static str>,
    /// Plain `http://` and `file://` allowed: only when the base URL was overridden by the caller or the environment.
    local: bool,
    installer_max: u64,
}

impl Config {
    /// `CODEYARD_UPDATE_URL` replaces the release download URL (a mirror, or a local server for testing). The signature
    /// check still applies: the keys cannot be overridden outside of tests.
    #[cfg_attr(test, allow(dead_code, reason = "only the real backend calls it; tests use fakes or `check_with`"))]
    pub(crate) fn from_env() -> Self {
        Self::new(std::env::var("CODEYARD_UPDATE_URL").ok().filter(|u| !u.is_empty()))
    }

    fn new(base_override: Option<String>) -> Self {
        let local = base_override.is_some();
        let base = base_override.unwrap_or_else(|| format!("{}/releases/latest/download", env!("CARGO_PKG_REPOSITORY")));
        Config { base: base.trim_end_matches('/').to_string(), keys: TRUSTED_KEYS.to_vec(), suffix: PLATFORM_SUFFIX, local, installer_max: INSTALLER_MAX }
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
}

/// The base `curl` command. Arguments are separate argv items, never a shell string. `-q` first: ignore `~/.curlrc`.
fn curl(cfg: &Config) -> Command {
    let mut c = Command::new("curl");
    c.args(["-q", "--fail", "--silent", "--show-error", "--location", "--max-redirs", "5", "--connect-timeout", "15", "--tlsv1.2"]);
    let (protos, redir) = if cfg.local { ("=https,http,file", "=https,http") } else { ("=https", "=https") };
    c.args(["--proto", protos, "--proto-redir", redir]);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn tool_err(tool: &'static str) -> impl FnOnce(io::Error) -> UpdateError {
    move |e| UpdateError::Tool { tool, detail: e.to_string() }
}

/// Stops a child we no longer need. It may have exited already, which is not worth reporting.
pub(crate) fn stop(child: &mut std::process::Child) {
    if let Err(e) = child.kill()
        && e.kind() != io::ErrorKind::InvalidInput
    {
        eprintln!("could not stop a helper process: {e}");
    }
    if let Err(e) = child.wait() {
        eprintln!("could not wait for a helper process: {e}");
    }
}

fn stderr_text(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).trim().to_string()
}

/// A small file, read through a bounded pipe: a hostile server cannot make us buffer more than `limit` bytes.
fn fetch(cfg: &Config, name: &str, limit: usize) -> Result<Vec<u8>, UpdateError> {
    let mut child = curl(cfg)
        .args(["--max-time", SMALL_TIMEOUT_SECS, "--url"])
        .arg(cfg.url(name))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(tool_err("curl"))?;
    let mut body = Vec::new();
    let read = match child.stdout.take() {
        Some(out) => out.take(limit as u64 + 1).read_to_end(&mut body),
        None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
    };
    if let Err(e) = read {
        stop(&mut child);
        return Err(UpdateError::Io { what: "read the update server's answer", source: e });
    }
    if body.len() > limit {
        stop(&mut child);
        return Err(UpdateError::TooLarge { file: name.to_string(), limit: limit as u64 });
    }
    let out = child.wait_with_output().map_err(tool_err("curl"))?;
    if !out.status.success() {
        return Err(UpdateError::Network(stderr_text(&out.stderr)));
    }
    Ok(body)
}

// --- Verification ---

/// `major.minor.patch`, digits only, no leading zeros: `0.9.0 < 0.10.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u32, u32, u32);

impl Version {
    fn parse(s: &str) -> Result<Self, UpdateError> {
        let bad = || UpdateError::BadVersion(s.chars().take(40).collect());
        let part = |p: &str| {
            let canonical = !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && (p.len() == 1 || !p.starts_with('0'));
            canonical.then(|| p.parse::<u32>().ok()).flatten()
        };
        let mut it = s.split('.');
        match (it.next().and_then(part), it.next().and_then(part), it.next().and_then(part), it.next()) {
            (Some(a), Some(b), Some(c), None) => Ok(Version(a, b, c)),
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn verify_signature(keys: &[[u8; 32]], message: &[u8], signature: &[u8]) -> Result<(), UpdateError> {
    let ok = signature.len() == SIG_LEN && keys.iter().any(|k| UnparsedPublicKey::new(&ED25519, k).verify(message, signature).is_ok());
    ok.then_some(()).ok_or(UpdateError::BadSignature)
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hashes a file in chunks (installers are tens of MB).
fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 << 10];
    loop {
        match f.read(&mut buf)? {
            0 => return Ok(hex(&hasher.finalize())),
            n => hasher.update(&buf[..n]),
        }
    }
}

pub(crate) fn verify_file(path: &Path, sha256: &str, name: &str) -> Result<(), UpdateError> {
    let got = sha256_file(path).map_err(io_err("read the downloaded file"))?;
    (got == sha256).then_some(()).ok_or_else(|| UpdateError::HashMismatch(name.to_string()))
}

/// The parsed, already signature-checked `SHA256SUMS.txt`: `<64 hex>  <file name>` per line.
struct Sums(Vec<(String, String)>);

impl Sums {
    fn parse(raw: &[u8]) -> Result<Self, UpdateError> {
        let text = std::str::from_utf8(raw).map_err(|_| UpdateError::NotText(SUMS.into()))?;
        let mut entries: Vec<(String, String)> = Vec::new();
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
            let bad = |why: &str| UpdateError::BadSums(format!("line {}: {why}", i + 1));
            let (hash, name) = line.split_once("  ").ok_or_else(|| bad("expected “<sha256>  <name>”"))?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(bad("not a SHA-256"));
            }
            if name.is_empty() || name.contains(['/', '\\']) || name.chars().any(char::is_control) {
                return Err(bad("unusable file name"));
            }
            if entries.iter().any(|(n, _)| n == name) {
                return Err(bad("listed twice"));
            }
            entries.push((name.to_string(), hash.to_ascii_lowercase()));
        }
        Ok(Sums(entries))
    }

    fn get(&self, name: &str) -> Result<&str, UpdateError> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, h)| h.as_str()).ok_or_else(|| UpdateError::MissingEntry(name.to_string()))
    }

    fn check(&self, name: &str, data: &[u8]) -> Result<(), UpdateError> {
        (sha256_hex(data) == self.get(name)?).then_some(()).ok_or_else(|| UpdateError::HashMismatch(name.to_string()))
    }

    /// The release version: every `Codeyard-<version>-<platform>` asset must carry the same one.
    fn version(&self) -> Result<Version, UpdateError> {
        let mut found: Option<Version> = None;
        for (name, _) in &self.0 {
            let Some(text) = VERSIONED_SUFFIXES
                .iter()
                .find_map(|s| name.strip_prefix("Codeyard-").or_else(|| name.strip_prefix("codeyard-"))?.strip_suffix(&format!("-{s}")))
            else {
                continue;
            };
            let v = Version::parse(text)?;
            if found.is_some_and(|f| f != v) {
                return Err(UpdateError::BadVersion(format!("mixed versions ({name})")));
            }
            found = Some(v);
        }
        found.ok_or_else(|| UpdateError::BadSums("no Codeyard release files listed".into()))
    }
}

// --- check ---

/// Is there a release newer than `current` (`env!("CARGO_PKG_VERSION")`)? `Ok(None)`: up to date (an equal or older
/// release is not offered, so a downgrade is never suggested). Blocking: a few small requests, at most ~30 s each.
#[cfg_attr(test, allow(dead_code, reason = "only the real backend calls it; tests use fakes or `check_with`"))]
pub fn check(current: &str) -> Result<Option<Release>, UpdateError> {
    check_with(&Config::from_env(), current)
}

pub(crate) fn check_with(cfg: &Config, current: &str) -> Result<Option<Release>, UpdateError> {
    let current = Version::parse(current)?;
    let sums_raw = fetch(cfg, SUMS, SUMS_MAX)?;
    let signature = fetch(cfg, SIG, SIG_LEN)?;
    // Nothing in the list is looked at before the signature is known to be good.
    verify_signature(&cfg.keys, &sums_raw, &signature)?;
    let sums = Sums::parse(&sums_raw)?;
    let version = sums.version()?;
    if version <= current {
        return Ok(None);
    }
    let notes_raw = fetch(cfg, NOTES, NOTES_MAX)?;
    sums.check(NOTES, &notes_raw)?;
    let notes = String::from_utf8(notes_raw).map_err(|_| UpdateError::NotText(NOTES.into()))?;
    let asset = cfg
        .suffix
        .map(|suffix| {
            let name = format!("Codeyard-{version}-{suffix}");
            Ok::<_, UpdateError>(Asset { sha256: sums.get(&name)?.to_string(), url: cfg.url(&name), name })
        })
        .transpose()?;
    Ok(Some(Release { version: version.to_string(), notes, asset }))
}

// --- download ---

/// Creates `dir` (and missing parents) readable by the owner only.
pub(crate) fn private_dir(dir: &Path) -> Result<(), UpdateError> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir).map_err(io_err("create a private folder"))
}

/// Removes a file or folder when dropped, unless `keep` was called; a failure is logged, not hidden.
pub(crate) struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    pub(crate) fn new(path: PathBuf) -> Self {
        RemoveOnDrop(Some(path))
    }

    pub(crate) fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let Some(p) = self.0.take() else { return };
        let r = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        if let Err(e) = r
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("could not remove {}: {e}", p.display());
        }
    }
}

#[cfg_attr(test, allow(dead_code, reason = "only the real backend calls it; tests use fakes or `check_with`"))]
fn updates_dir() -> Result<PathBuf, UpdateError> {
    let state = crate::workbench::state_path().and_then(|p| p.parent().map(Path::to_path_buf));
    state.map(|d| d.join("updates")).ok_or_else(|| UpdateError::Io { what: "find the state folder", source: io::ErrorKind::NotFound.into() })
}

/// Downloads the installer of `release` into a fresh private folder and checks it against the signed hash.
/// Blocking; `progress` is called about ten times a second on the calling thread. Setting `cancel` stops the
/// transfer within ~100 ms (`Cancelled`). The result is only ever returned for a file whose hash matched.
#[cfg_attr(test, allow(dead_code, reason = "only the real backend calls it; tests use fakes or `check_with`"))]
pub fn download(release: &Release, progress: &mut dyn FnMut(Progress), cancel: &AtomicBool) -> Result<Downloaded, UpdateError> {
    download_in(&Config::from_env(), &updates_dir()?, env!("CARGO_PKG_VERSION"), release, progress, cancel)
}

pub(crate) fn download_in(
    cfg: &Config,
    dir: &Path,
    current: &str,
    release: &Release,
    progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<Downloaded, UpdateError> {
    let (offered, current) = (Version::parse(&release.version)?, Version::parse(current)?);
    if offered <= current {
        return Err(UpdateError::NotNewer { offered: offered.to_string(), current: current.to_string() });
    }
    let asset = release.asset.as_ref().ok_or(UpdateError::NoInstaller)?;
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_err("clear the previous update")(e)),
        _ => {}
    }
    private_dir(dir)?;
    let wipe = RemoveOnDrop::new(dir.to_path_buf());
    let (dest, headers) = (dir.join(&asset.name), dir.join("headers.txt"));

    let mut child = curl(cfg)
        .args(["--max-time", INSTALLER_TIMEOUT_SECS, "--max-filesize"])
        .arg(cfg.installer_max.to_string())
        .arg("--dump-header")
        .arg(&headers)
        .arg("--output")
        .arg(&dest)
        .arg("--url")
        .arg(&asset.url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(tool_err("curl"))?;
    let too_large = || UpdateError::TooLarge { file: asset.name.clone(), limit: cfg.installer_max };
    let mut total = None;
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            stop(&mut child);
            return Err(UpdateError::Cancelled);
        }
        // A stat error only costs one progress tick; the transfer's own outcome is judged from curl's exit status.
        let done = std::fs::metadata(&dest).map_or(0, |m| m.len());
        if done > cfg.installer_max {
            stop(&mut child);
            return Err(too_large());
        }
        total = total.or_else(|| std::fs::read_to_string(&headers).ok().and_then(|h| final_content_length(&h)));
        progress(Progress { done, total });
        if let Some(status) = child.try_wait().map_err(tool_err("curl"))? {
            break status;
        }
        std::thread::sleep(POLL);
    };
    if !status.success() {
        let mut msg = String::new();
        if let Some(e) = child.stderr.take() {
            e.take(4096).read_to_string(&mut msg).map_err(io_err("read curl's error message"))?;
        }
        // curl exit 63: "Maximum file size exceeded".
        return Err(if status.code() == Some(63) { too_large() } else { UpdateError::Network(msg.trim().to_string()) });
    }
    verify_file(&dest, &asset.sha256, &asset.name)?;
    let size = std::fs::metadata(&dest).map_err(io_err("read the downloaded file"))?.len();
    progress(Progress { done: size, total: Some(size) });
    wipe.keep();
    Ok(Downloaded { release: release.clone(), path: dest })
}

/// `Content-Length` of the last response in a `--dump-header` file (redirects leave one block per hop),
/// once that response is a success; `None` while still redirecting or when the server did not say.
fn final_content_length(headers: &str) -> Option<u64> {
    let block = headers.replace("\r\n", "\n").split("\n\n").filter(|b| !b.trim().is_empty()).last()?.to_string();
    let mut lines = block.lines();
    let code = lines.next()?.split_whitespace().nth(1)?;
    if !code.starts_with('2') {
        return None;
    }
    lines.find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))?.1.trim().parse().ok())
}

impl Downloaded {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
pub(crate) mod tests;
