//! Which hosts a request may reach without asking: the policy behind `http.confirmRequests`. Shared by the `.http` client and
//! schema downloads (so a host approved once in a session is approved for both). The gate only keeps the policy and the
//! session's answers; asking the user is the caller's job.

use std::collections::HashSet;
use std::sync::Mutex;

/// `never`: no extra question. `remote`: ask once per session for each host that is not on this computer. `always`: ask for
/// every request (schema downloads: once per host and session, they are repeated in the background).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Confirm {
    Never,
    #[default]
    Remote,
    Always,
}

impl Confirm {
    pub fn parse(s: &str) -> Option<Confirm> {
        Some(match s {
            "never" => Confirm::Never,
            "remote" => Confirm::Remote,
            "always" => Confirm::Always,
            _ => return None,
        })
    }
}

/// The host of an http(s) URL, lower-case, without port, user info, brackets or a trailing dot. `None` for anything else
/// (callers treat an unreadable host as one that needs approval).
/// `https://trusted.example@evil.example/` is `evil.example`: the part before the last `@` is user info.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).or_else(|| {
        let (scheme, rest) = url.split_once("://")?;
        (scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")).then_some(rest)
    })?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    // A backslash in the authority is read differently by different clients (`a\@b`): refuse to guess.
    if authority.contains('\\') {
        return None;
    }
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(v6) => v6.split_once(']')?.0,
        None => host_port.split(':').next().unwrap_or(host_port),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// A host on this computer: `localhost` (and `*.localhost`), loopback addresses, the unspecified address.
pub fn is_local_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host == "localhost" || host.ends_with(".localhost") || host == "::1" || host == "::" || host == "0.0.0.0" {
        return true;
    }
    host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

#[derive(Default)]
struct Inner {
    mode: Confirm,
    approved: HashSet<String>,
    refused: HashSet<String>,
    pending: Vec<String>,
}

/// The session's policy and answers. Cheap to share (`Arc`); all methods take `&self`.
#[derive(Default)]
pub struct HostGate {
    inner: Mutex<Inner>,
}

impl HostGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_mode(&self, mode: Confirm) {
        self.lock().mode = mode;
    }

    pub fn mode(&self) -> Confirm {
        self.lock().mode
    }

    /// Must the user approve `host` first (it is not approved yet in this session)?
    pub fn needs_approval(&self, host: &str) -> bool {
        let g = self.lock();
        match g.mode {
            Confirm::Never => false,
            Confirm::Remote => !is_local_host(host) && !g.approved.contains(host),
            Confirm::Always => !g.approved.contains(host),
        }
    }

    pub fn approve(&self, host: &str) {
        let mut g = self.lock();
        g.refused.remove(host);
        g.approved.insert(host.to_string());
    }

    /// The user said no to a background download from `host`: do not ask again this session.
    pub fn refuse(&self, host: &str) {
        self.lock().refused.insert(host.to_string());
    }

    pub fn is_refused(&self, host: &str) -> bool {
        self.lock().refused.contains(host)
    }

    /// A background download wants `host`: remember to ask (once, however many files want it).
    pub fn request(&self, host: &str) {
        let mut g = self.lock();
        if !g.pending.iter().any(|h| h == host) && !g.refused.contains(host) {
            g.pending.push(host.to_string());
        }
    }

    pub fn take_pending(&self) -> Vec<String> {
        std::mem::take(&mut self.lock().pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_read_the_way_a_client_connects() {
        for (url, host) in [
            ("https://api.example.com/v1?x=1#f", "api.example.com"),
            ("http://API.Example.COM:8080/", "api.example.com"),
            ("https://trusted.example@evil.example/path", "evil.example"),
            ("https://user:pass@evil.example:443/", "evil.example"),
            ("http://[::1]:3000/x", "::1"),
            ("http://localhost./x", "localhost"),
            ("HTTPS://Shout.example/", "shout.example"),
            ("https://host.example?q=@x", "host.example"),
        ] {
            assert_eq!(host_of(url).as_deref(), Some(host), "{url}");
        }
        for url in
            ["ftp://x/", "file:///etc/passwd", "https:///nohost", "api.example.com", "https://@/", "http://[::1/x", "https://evil.example\\@trusted.example/"]
        {
            assert_eq!(host_of(url), None, "{url}");
        }
    }

    #[test]
    fn only_this_computer_is_local() {
        for h in ["localhost", "foo.localhost", "127.0.0.1", "127.1.2.3", "::1", "0.0.0.0"] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["localhost.example.com", "example.com", "128.0.0.1", "10.0.0.5", "192.168.1.1", "127.0.0.1.example.com", "notlocalhost"] {
            assert!(!is_local_host(h), "{h}");
        }
    }

    #[test]
    fn the_modes() {
        let g = HostGate::default();
        assert_eq!(g.mode(), Confirm::Remote, "the default asks for new remote hosts");
        assert!(g.needs_approval("api.example.com"));
        assert!(!g.needs_approval("localhost") && !g.needs_approval("127.0.0.1"));
        g.approve("api.example.com");
        assert!(!g.needs_approval("api.example.com"));
        assert!(g.needs_approval("other.example.com"), "approval is per host");
        g.set_mode(Confirm::Always);
        assert!(g.needs_approval("localhost"), "always asks for local hosts too");
        assert!(!g.needs_approval("api.example.com"), "approved hosts are remembered for the session");
        g.set_mode(Confirm::Never);
        assert!(!g.needs_approval("brand-new.example.com"));
        assert_eq!(Confirm::parse("remote"), Some(Confirm::Remote));
        assert_eq!(Confirm::parse("sometimes"), None);
    }

    #[test]
    fn pending_hosts_are_asked_once_and_a_refusal_is_remembered() {
        let g = HostGate::default();
        g.request("a.example");
        g.request("a.example");
        g.request("b.example");
        assert_eq!(g.take_pending(), ["a.example", "b.example"]);
        assert!(g.take_pending().is_empty());
        g.refuse("a.example");
        g.request("a.example");
        assert!(g.take_pending().is_empty(), "refused hosts are not asked again");
        assert!(g.is_refused("a.example"));
        g.approve("a.example");
        assert!(!g.is_refused("a.example"), "approving later wins");
    }
}
