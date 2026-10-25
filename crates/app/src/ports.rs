//! Port forwarding: find the ports listening on the server and bind them to a local port.
//! Binding goes through ssh's open ControlMaster connection (`host::ssh::forward_port`).

use host::Host;

/// `/proc/net/tcp` and `tcp6`: local ports in state 0A (LISTEN).
pub fn parse_proc_net(text: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = text
        .lines()
        .filter_map(|l| {
            let mut cols = l.split_whitespace();
            let _sl = cols.next()?;
            let local = cols.next()?;
            let _remote = cols.next()?;
            let state = cols.next()?;
            if state != "0A" {
                return None;
            }
            u16::from_str_radix(local.rsplit_once(':')?.1, 16).ok()
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Output of `lsof -nP -iTCP -sTCP:LISTEN -Fn` (macOS servers): `n*:3000`, `n127.0.0.1:8080`.
pub fn parse_lsof(text: &str) -> Vec<u16> {
    let mut ports: Vec<u16> = text.lines().filter_map(|l| l.strip_prefix('n')).filter_map(|n| n.rsplit_once(':')?.1.parse().ok()).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Ports listening on the server (empty if none are found).
pub fn detect(host: &dyn Host) -> Vec<u16> {
    if let Ok(out) = host.exec("cat", &["/proc/net/tcp", "/proc/net/tcp6"], None, None)
        && out.ok()
    {
        return parse_proc_net(&out.stdout_str());
    }
    match host.exec("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fn"], None, None) {
        Ok(out) if out.ok() => parse_lsof(&out.stdout_str()),
        _ => Vec::new(),
    }
}

/// Auto-forward everything except privileged ports and ssh.
pub fn worth_forwarding(port: u16) -> bool {
    port >= 1024 && port != 22
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listening_ports() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1\n\
   1: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 2 1\n\
   2: 0100007F:0BB8 0100007F:D2F0 01 00000000:00000000 00:00000000 00000000  1000        0 3 1\n\
   3: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 0 0 0 0 0 4 1\n";
        assert_eq!(parse_proc_net(tcp), vec![22, 3000, 8080], "established (01) connections are not counted");
        assert_eq!(parse_lsof("p123\nf5\nn*:3000\nn127.0.0.1:5432\nn[::1]:3000\n"), vec![3000, 5432]);
        assert!(!worth_forwarding(22) && !worth_forwarding(80) && worth_forwarding(5173));
    }
}
