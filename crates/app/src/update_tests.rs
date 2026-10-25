//! Tests for `update.rs`: no real network (a signed release is written to a temp folder and fetched through `file://`),
//! a throw-away signing key, no real state folder.

use std::sync::atomic::AtomicU32;

use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

use super::*;

const DMG: &str = "macos-arm64.dmg";

/// A fresh folder under the temp dir, removed when the guard drops.
pub(crate) fn scratch(name: &str) -> (PathBuf, RemoveOnDrop) {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("codeyard-update-{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).unwrap();
    (dir.clone(), RemoveOnDrop::new(dir))
}

fn keypair() -> Ed25519KeyPair {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

/// A release folder as the release workflow lays it out, signed with a key only this test knows.
struct Fixture {
    dir: PathBuf,
    key: Ed25519KeyPair,
    _guard: RemoveOnDrop,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let (dir, _guard) = scratch(name);
        Fixture { dir, key: keypair(), _guard }
    }

    fn cfg(&self) -> Config {
        Config {
            base: format!("file://{}", self.dir.display()),
            keys: vec![self.key.public_key().as_ref().try_into().unwrap()],
            suffix: Some(DMG),
            local: true,
            installer_max: 1 << 20,
        }
    }

    fn put(&self, name: &str, data: &[u8]) {
        std::fs::write(self.dir.join(name), data).unwrap();
    }

    fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.join(name)).unwrap()
    }

    /// Writes `SHA256SUMS.txt` and its signature from literal text.
    fn sign(&self, sums: &[u8]) {
        self.put("SHA256SUMS.txt", sums);
        self.put("SHA256SUMS.txt.sig", self.key.sign(sums).as_ref());
    }

    /// Writes the files and a signed list of exactly those.
    fn publish_files(&self, files: &[(String, &[u8])]) {
        let mut sums = String::new();
        for (name, data) in files {
            self.put(name, data);
            sums += &format!("{}  {name}\n", sha256_hex(data));
        }
        self.sign(sums.as_bytes());
    }

    fn publish_with_notes(&self, version: &str, installer: &[u8], notes: &[u8]) {
        let f = |suffix: &str| format!("Codeyard-{version}-{suffix}");
        self.publish_files(&[
            (f(DMG), installer),
            (format!("codeyard-{version}-macos-arm64.tar.gz"), b"tar"),
            (f("windows-x64-setup.exe"), b"exe"),
            (f("windows-x64.zip"), b"zip"),
            ("RELEASE_NOTES.md".into(), notes),
        ]);
    }

    fn publish(&self, version: &str, installer: &[u8]) {
        self.publish_with_notes(version, installer, b"# What is new\n\n- Updates\n");
    }
}

fn check(f: &Fixture, current: &str) -> Result<Option<Release>, UpdateError> {
    check_with(&f.cfg(), current)
}

fn download(f: &Fixture, release: &Release) -> Result<Downloaded, UpdateError> {
    download_in(&f.cfg(), &f.dir.join("dl"), "0.1.0", release, &mut |_| {}, &AtomicBool::new(false))
}

fn err<T: std::fmt::Debug>(r: Result<T, UpdateError>) -> UpdateError {
    r.expect_err("expected an error")
}

#[test]
fn update_happy_path_checks_downloads_and_verifies() {
    let f = Fixture::new("happy");
    f.publish("0.2.0", b"installer bytes");
    let release = check(&f, "0.1.0").unwrap().expect("a newer release");
    assert_eq!(release.version, "0.2.0");
    assert_eq!(release.notes, "# What is new\n\n- Updates\n");
    let asset = release.asset.clone().unwrap();
    assert_eq!(asset.name, "Codeyard-0.2.0-macos-arm64.dmg");
    assert_eq!(asset.sha256, sha256_hex(b"installer bytes"));
    assert!(asset.url.ends_with("/Codeyard-0.2.0-macos-arm64.dmg"));

    let mut seen = Vec::new();
    let dl = download_in(&f.cfg(), &f.dir.join("dl"), "0.1.0", &release, &mut |p| seen.push(p), &AtomicBool::new(false)).unwrap();
    assert_eq!(std::fs::read(dl.path()).unwrap(), b"installer bytes");
    assert_eq!(*seen.last().unwrap(), Progress { done: 15, total: Some(15) });
    assert!(seen.windows(2).all(|w| w[0].done <= w[1].done));
    assert_eq!(dl.release, release);
}

#[test]
fn update_equal_and_older_releases_are_not_offered() {
    let f = Fixture::new("notnewer");
    f.publish("0.10.0", b"x");
    assert_eq!(check(&f, "0.10.0").unwrap(), None, "equal is not newer");
    assert_eq!(check(&f, "0.11.0").unwrap(), None, "never a downgrade");
    assert!(check(&f, "0.9.0").unwrap().is_some(), "0.9.0 < 0.10.0 numerically, not as text");
    assert!(Version::parse("0.9.0").unwrap() < Version::parse("0.10.0").unwrap());
    assert!(Version::parse("1.0.0").unwrap() > Version::parse("0.99.99").unwrap());
}

#[test]
fn update_download_refuses_a_release_that_is_not_newer() {
    let f = Fixture::new("downgrade");
    f.publish("0.2.0", b"x");
    let release = check(&f, "0.1.0").unwrap().unwrap();
    for current in ["0.2.0", "0.3.0"] {
        let e = err(download_in(&f.cfg(), &f.dir.join("dl"), current, &release, &mut |_| {}, &AtomicBool::new(false)));
        assert!(matches!(e, UpdateError::NotNewer { .. }), "{e}");
        assert_eq!(e.kind(), ErrorKind::Untrusted);
    }
    assert!(!f.dir.join("dl").exists(), "nothing was fetched");
}

#[test]
fn update_bogus_signature_is_rejected() {
    let f = Fixture::new("badsig");
    f.publish("0.2.0", b"x");
    let other = keypair();
    f.put("SHA256SUMS.txt.sig", other.sign(&f.read("SHA256SUMS.txt")).as_ref());
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::BadSignature));
    f.put("SHA256SUMS.txt.sig", &[0; 64]);
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::BadSignature));
    f.put("SHA256SUMS.txt.sig", &f.key.sign(b"something else").as_ref()[..63]);
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::BadSignature), "a short signature");
    f.put("SHA256SUMS.txt.sig", &[f.key.sign(&f.read("SHA256SUMS.txt")).as_ref(), &[0]].concat());
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::TooLarge { .. }), "a long signature");
}

#[test]
fn update_a_key_that_is_not_trusted_is_rejected() {
    let f = Fixture::new("untrusted");
    f.publish("0.2.0", b"x");
    let mut cfg = f.cfg();
    cfg.keys = vec![[7; 32]];
    assert!(matches!(err(check_with(&cfg, "0.1.0")), UpdateError::BadSignature));
    cfg.keys.push(f.key.public_key().as_ref().try_into().unwrap());
    assert!(check_with(&cfg, "0.1.0").unwrap().is_some(), "a second key in the list is enough (rotation)");
}

#[test]
fn update_tampered_sums_line_is_a_signature_failure_not_a_parse_failure() {
    let f = Fixture::new("tamper-sums");
    f.publish("0.2.0", b"x");
    let sums = String::from_utf8(f.read("SHA256SUMS.txt")).unwrap();
    let tampered = sums.replacen(&sha256_hex(b"x")[..1], if sha256_hex(b"x").starts_with('0') { "1" } else { "0" }, 1);
    assert_ne!(tampered, sums);
    f.put("SHA256SUMS.txt", tampered.as_bytes());
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::BadSignature));
    // Unsigned garbage must not reach the parser either.
    f.put("SHA256SUMS.txt", b"not a list\n");
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::BadSignature));
}

#[test]
fn update_signed_but_malformed_sums_are_rejected_by_the_parser() {
    let f = Fixture::new("badsums");
    let h = "a".repeat(64);
    for (text, why) in [
        (format!("{h} Codeyard-0.2.0-macos-arm64.dmg\n"), "one space"),
        (format!("{}  Codeyard-0.2.0-macos-arm64.dmg\n", "z".repeat(64)), "not hex"),
        (format!("{h}  a/b\n"), "path separator"),
        (format!("{h}  x\n{h}  x\n"), "duplicate"),
        (format!("{h}  Codeyard-0.2.0-macos-arm64.dmg\n").replace("0.2.0", "0.2.0\u{7}"), "control character"),
        (format!("{h}  something-else.txt\n"), "no release files"),
    ] {
        f.sign(text.as_bytes());
        let e = err(check(&f, "0.1.0"));
        assert!(matches!(e, UpdateError::BadSums(_) | UpdateError::BadVersion(_)), "{why}: {e}");
    }
    f.sign(&[0xff, 0xfe]);
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::NotText(_)));
}

#[test]
fn update_malformed_versions_are_rejected() {
    let f = Fixture::new("badversion");
    for v in ["1.2", "1.2.x", "1.2.3-rc1", "01.2.3", "1.2.3.4", "1.2.-3", "+1.2.3", "1..3", "", "99999999999.0.0"] {
        f.publish_files(&[(format!("Codeyard-{v}-{DMG}"), b"x"), ("RELEASE_NOTES.md".into(), b"n")]);
        let e = err(check(&f, "0.1.0"));
        assert!(matches!(e, UpdateError::BadVersion(_)), "{v:?}: {e}");
    }
    f.publish_files(&[(format!("Codeyard-0.2.0-{DMG}"), b"x"), ("Codeyard-0.3.0-windows-x64.zip".into(), b"y")]);
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::BadVersion(_)), "assets of two versions in one release");
    assert!(matches!(err(check(&f, "0.1.0-dev")), UpdateError::BadVersion(_)), "our own version must parse too");
}

#[test]
fn update_oversized_files_are_rejected() {
    let f = Fixture::new("oversize");
    f.sign(&vec![b'a'; 64 * 1024 + 1]);
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::TooLarge { limit: 65536, .. }));

    f.publish_with_notes("0.2.0", b"x", &vec![b'n'; 256 * 1024 + 1]);
    let e = err(check(&f, "0.1.0"));
    assert!(matches!(&e, UpdateError::TooLarge { file, .. } if file == "RELEASE_NOTES.md"), "{e}");

    f.publish("0.2.0", &[0; 100]);
    let mut cfg = f.cfg();
    let release = check_with(&cfg, "0.1.0").unwrap().unwrap();
    cfg.installer_max = 10;
    let e = err(download_in(&cfg, &f.dir.join("dl"), "0.1.0", &release, &mut |_| {}, &AtomicBool::new(false)));
    assert!(matches!(&e, UpdateError::TooLarge { limit: 10, .. }), "{e}");
    assert!(!f.dir.join("dl").exists(), "the partial file is gone");
}

#[test]
fn update_tampered_notes_and_installer_are_rejected() {
    let f = Fixture::new("tamper-files");
    f.publish("0.2.0", b"good");
    f.put("RELEASE_NOTES.md", b"# evil notes");
    let e = err(check(&f, "0.1.0"));
    assert!(matches!(&e, UpdateError::HashMismatch(n) if n == "RELEASE_NOTES.md"), "{e}");

    f.publish("0.2.0", b"good");
    let release = check(&f, "0.1.0").unwrap().unwrap();
    f.put("Codeyard-0.2.0-macos-arm64.dmg", b"evil");
    let e = err(download(&f, &release));
    assert!(matches!(&e, UpdateError::HashMismatch(n) if n.ends_with(".dmg")), "{e}");
    assert_eq!(e.kind(), ErrorKind::Untrusted);
    assert!(!f.dir.join("dl").exists(), "the rejected file is removed");
}

#[test]
fn update_notes_must_be_utf8_and_listed() {
    let f = Fixture::new("notes");
    f.publish_with_notes("0.2.0", b"x", &[0xff, 0xfe, 0x00]);
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::NotText(_)));

    // Served but not in the signed list: not trusted.
    f.publish_files(&[(format!("Codeyard-0.2.0-{DMG}"), b"x")]);
    f.put("RELEASE_NOTES.md", b"unsigned notes");
    assert!(matches!(err(check(&f, "0.1.0")), UpdateError::MissingEntry(n) if n == "RELEASE_NOTES.md"));
}

#[test]
fn update_missing_platform_installer_in_signed_list_is_an_error_and_no_platform_installer_is_none() {
    let f = Fixture::new("noasset");
    f.publish_files(&[("Codeyard-0.2.0-windows-x64.zip".into(), b"z"), ("RELEASE_NOTES.md".into(), b"n")]);
    let e = err(check(&f, "0.1.0"));
    assert!(matches!(&e, UpdateError::MissingEntry(n) if n == "Codeyard-0.2.0-macos-arm64.dmg"), "{e}");
    let mut cfg = f.cfg();
    cfg.suffix = None; // Linux
    let release = check_with(&cfg, "0.1.0").unwrap().unwrap();
    assert_eq!(release.asset, None);
    let e = err(download_in(&cfg, &f.dir.join("dl"), "0.1.0", &release, &mut |_| {}, &AtomicBool::new(false)));
    assert!(matches!(e, UpdateError::NoInstaller));
}

#[test]
fn update_a_missing_file_is_a_network_error_the_user_can_retry() {
    let f = Fixture::new("missing");
    let e = err(check(&f, "0.1.0"));
    assert!(matches!(e, UpdateError::Network(_)), "{e}");
    assert_eq!(e.kind(), ErrorKind::Network);
}

#[test]
fn update_cancel_stops_the_download_and_leaves_nothing() {
    let f = Fixture::new("cancel");
    f.publish("0.2.0", b"x");
    let release = check(&f, "0.1.0").unwrap().unwrap();
    let cancel = AtomicBool::new(true);
    let e = err(download_in(&f.cfg(), &f.dir.join("dl"), "0.1.0", &release, &mut |_| {}, &cancel));
    assert!(matches!(e, UpdateError::Cancelled));
    assert_eq!(e.kind(), ErrorKind::Cancelled);
    assert!(!f.dir.join("dl").exists());
}

#[test]
fn update_download_clears_the_previous_attempt() {
    let f = Fixture::new("fresh");
    f.publish("0.2.0", b"x");
    let release = check(&f, "0.1.0").unwrap().unwrap();
    std::fs::create_dir_all(f.dir.join("dl")).unwrap();
    std::fs::write(f.dir.join("dl/stale.dmg"), b"old").unwrap();
    let dl = download(&f, &release).unwrap();
    assert!(!f.dir.join("dl/stale.dmg").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dl.path().parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    }
}

#[test]
fn update_default_source_is_the_repository_over_https_and_curl_is_locked_down() {
    let cfg = Config::new(None);
    assert_eq!(cfg.base, "https://github.com/doancan/codeyard/releases/latest/download");
    assert!(!cfg.local);
    assert_eq!(cfg.keys, TRUSTED_KEYS);
    assert_eq!(cfg.url("SHA256SUMS.txt"), "https://github.com/doancan/codeyard/releases/latest/download/SHA256SUMS.txt");
    let args: Vec<String> = curl(&cfg).get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(args[0], "-q", "ignore ~/.curlrc");
    let pair = |k: &str, v: &str| args.windows(2).any(|w| w[0] == k && w[1] == v);
    assert!(pair("--proto", "=https") && pair("--proto-redir", "=https") && pair("--max-redirs", "5"), "{args:?}");
    assert!(args.contains(&"--tlsv1.2".into()) && args.contains(&"--fail".into()));

    let local = Config::new(Some("http://127.0.0.1:1/x/".into()));
    assert!(local.local);
    assert_eq!(local.base, "http://127.0.0.1:1/x", "trailing slash dropped");
    assert_eq!(local.keys, TRUSTED_KEYS, "the keys cannot be overridden");
}

#[test]
fn update_trusted_key_list_is_not_empty_and_decodes() {
    assert!(!TRUSTED_KEYS.is_empty());
    assert_eq!(TRUSTED_KEYS[0][0], 0x47);
    assert_eq!(TRUSTED_KEYS[0][31], 0x7c);
}

#[test]
fn update_content_length_comes_from_the_last_successful_response() {
    let redirect = "HTTP/2 302\r\nlocation: https://x/y\r\ncontent-length: 0\r\n\r\n";
    assert_eq!(final_content_length(redirect), None, "still redirecting");
    assert_eq!(final_content_length(&format!("{redirect}HTTP/2 200\r\nContent-Length: 1234\r\n\r\n")), Some(1234));
    assert_eq!(final_content_length("HTTP/1.1 200 OK\ncontent-type: x\n\n"), None, "no length sent");
    assert_eq!(final_content_length(""), None);
}

#[test]
fn update_policy_parses_and_prints() {
    for p in [Policy::Ask, Policy::Daily, Policy::Manual] {
        assert_eq!(p.as_str().parse::<Policy>(), Ok(p));
    }
    assert_eq!("Daily".parse::<Policy>(), Err(UnknownPolicy));
    assert_eq!("".parse::<Policy>(), Err(UnknownPolicy));
}

#[test]
fn update_daily_check_is_due_after_a_day_and_handles_a_backwards_clock() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 86_400);
    let ago = |s| Some(now - Duration::from_secs(s));
    assert!(should_check_now(None, now, Policy::Daily));
    assert!(!should_check_now(ago(60), now, Policy::Daily));
    assert!(!should_check_now(ago(86_399), now, Policy::Daily));
    assert!(should_check_now(ago(86_400), now, Policy::Daily));
    assert!(should_check_now(Some(now + Duration::from_secs(5)), now, Policy::Daily), "clock went backwards: check and re-record");
    for p in [Policy::Ask, Policy::Manual] {
        assert!(!should_check_now(None, now, p) && !should_check_now(ago(10 * 86_400), now, p), "{p:?} never checks by itself");
    }
}

#[test]
fn update_errors_read_in_plain_english_and_carry_a_kind() {
    use UpdateError::*;
    let io = || io::Error::other("disk full");
    let cases = [
        (Network("timed out".into()), ErrorKind::Network),
        (BadSignature, ErrorKind::Untrusted),
        (HashMismatch("a.dmg".into()), ErrorKind::Untrusted),
        (TooLarge { file: "a.dmg".into(), limit: 400 << 20 }, ErrorKind::Untrusted),
        (NotNewer { offered: "0.1.0".into(), current: "0.2.0".into() }, ErrorKind::Untrusted),
        (NoWritePermission("/Applications".into()), ErrorKind::Install),
        (BundleMismatch("wrong".into()), ErrorKind::Install),
        (MountFailed("busy".into()), ErrorKind::Install),
        (RollbackFailed { app: "/A.app".into(), backup: "/.old.app".into(), cause: io() }, ErrorKind::Install),
        (Cancelled, ErrorKind::Cancelled),
    ];
    for (e, kind) in cases {
        assert_eq!(e.kind(), kind, "{e:?}");
        let text = e.to_string();
        assert!(text.ends_with('.') || text.contains(": "), "{text}");
        assert!(!text.contains("{") && !text.contains("Error"), "{text}");
    }
    assert!(RollbackFailed { app: "/A.app".into(), backup: "/.old.app".into(), cause: io() }.to_string().contains("/.old.app"));
}

/// The key the app trusts and the key the release workflow verifies with must be the same one: a mismatch would make every
/// installed app reject every update, silently.
#[test]
fn the_embedded_key_is_the_one_the_release_scripts_verify_with() {
    fn base64(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0);
        for c in text.bytes().filter(|c| *c != b'=') {
            let v = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().position(|x| *x == c).expect("base64 character");
            acc = (acc << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        out
    }
    let pem = include_str!("../../../scripts/update-public-key.pem");
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64(&body);
    // SubjectPublicKeyInfo of an Ed25519 key: a 12-byte header, then the 32 raw bytes.
    assert_eq!(der.len(), 44, "unexpected key encoding");
    assert_eq!(der[..12], [0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00]);
    assert_eq!(TRUSTED_KEYS[0], der[12..], "scripts/update-public-key.pem and TRUSTED_KEYS differ");
}
