//! Auto-discovery of a *local* Bee node's log output.
//!
//! Bee has no log-streaming API and no "log to file" option — its
//! log lines only ever exist on the `bee` process's stdout. When
//! bee-tui *connects* to a Bee it didn't spawn (the default path),
//! it has to go *find* where that stdout went. This module does
//! that, on Linux, via `/proc`:
//!
//! 1. Parse the node URL. Discovery only applies to a **local**
//!    (loopback) host — there's no `/proc` for a remote machine.
//! 2. Find the PID listening on the API port: collect the
//!    listening-socket inodes for that port from `/proc/net/tcp{,6}`,
//!    then scan `/proc/<pid>/fd/*` for the process holding one.
//! 3. Classify `/proc/<pid>/fd/1` (Bee logs to stdout):
//!    - a **regular file** → tail it directly (most reliable).
//!    - a **tty** / **/dev/null** → can't capture; return an
//!      operator-facing explanation + fix.
//!    - a **pipe/socket** → consult `/proc/<pid>/cgroup`: a docker
//!      scope yields `docker logs -f`, a systemd `.service` yields
//!      `journalctl -u …`. Otherwise it's an opaque pipe we can't
//!      identify — tell the operator to set `log_command`.
//!
//! Everything here is best-effort: any unreadable `/proc` entry is
//! skipped, and the worst case is [`DiscoveryResult::NotApplicable`]
//! (fall through to the generic "no log source" placeholder). It is
//! only consulted when the operator has *not* set an explicit
//! `log_file` / `log_command` — explicit config always wins.

use std::path::PathBuf;

/// A resolved external bee-log source — the input the cockpit feeds
/// to the tailer. Either a file to poll-tail from EOF, or a shell
/// command whose stdout to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeeLogSource {
    File(PathBuf),
    Command(String),
}

/// Outcome of an auto-discovery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryResult {
    /// Found a usable log source — tail it.
    Found(BeeLogSource),
    /// A local Bee was found, but its log can't be captured. The
    /// string is an operator-facing explanation + how to fix it;
    /// it is surfaced on the empty Bee-side log tabs.
    Unsupported(String),
    /// Discovery doesn't apply or didn't find anything — non-local
    /// URL, non-Linux host, or no listening process found. Silent:
    /// the caller falls through to the generic placeholder.
    NotApplicable,
}

/// Split a URL into `(host, Option<port>)`. Hand-rolled — Bee URLs
/// are a simple, known shape and bee-tui has no URL-parser dep.
fn split_host_port(url: &str) -> Option<(String, Option<u16>)> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return None;
    }
    // Drop any `userinfo@` prefix.
    let authority = authority
        .rsplit_once('@')
        .map(|(_, a)| a)
        .unwrap_or(authority);
    if let Some(stripped) = authority.strip_prefix('[') {
        // `[ipv6]` or `[ipv6]:port`
        let (host, after) = stripped.split_once(']')?;
        let port = after.strip_prefix(':').and_then(|p| p.parse().ok());
        Some((host.to_string(), port))
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        Some((host.to_string(), port.parse().ok()))
    } else {
        Some((authority.to_string(), None))
    }
}

/// True when `host` names the local machine (loopback). Hostnames
/// and LAN IPs are deliberately treated as non-local — `/proc`-based
/// discovery can only reach processes on this machine.
fn is_local_host(host: &str) -> bool {
    host == "localhost"
        || host == "::1"
        || host == "0.0.0.0"
        || host == "::"
        || host.starts_with("127.")
}

#[cfg(target_os = "linux")]
pub use linux::discover;

#[cfg(not(target_os = "linux"))]
pub fn discover(_url: &str) -> DiscoveryResult {
    // No `/proc` to inspect — explicit `log_file` / `log_command`
    // config is the only path on non-Linux hosts.
    DiscoveryResult::NotApplicable
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use super::{BeeLogSource, DiscoveryResult, is_local_host, split_host_port};

    /// What `/proc/<pid>/fd/1` points at.
    #[derive(Debug, PartialEq, Eq)]
    enum FdTarget {
        File(PathBuf),
        Tty(String),
        DevNull,
        /// `pipe:[…]`, `socket:[…]`, `anon_inode:[…]`, a deleted
        /// file, etc. — opaque, needs the cgroup to interpret.
        Other(String),
    }

    /// What `/proc/<pid>/cgroup` says is managing the process.
    #[derive(Debug, PartialEq, Eq)]
    enum CgroupSource {
        Docker(String),
        Systemd { unit: String, user: bool },
        None,
    }

    /// Discover a local Bee's log source from its API URL. See the
    /// module docs for the full algorithm.
    pub fn discover(url: &str) -> DiscoveryResult {
        let Some((host, port)) = split_host_port(url) else {
            return DiscoveryResult::NotApplicable;
        };
        if !is_local_host(&host) {
            return DiscoveryResult::NotApplicable;
        }
        // Bee's default API port — used when the URL omits one.
        let port = port.unwrap_or(1633);

        let Some(pid) = find_listener_pid(port) else {
            return DiscoveryResult::NotApplicable;
        };

        // fd/1 first: a regular file is the most reliable source and
        // works regardless of how the process is managed.
        match classify_fd_target(pid) {
            Some(FdTarget::File(path)) => {
                return DiscoveryResult::Found(BeeLogSource::File(path));
            }
            Some(FdTarget::Tty(tty)) => {
                return DiscoveryResult::Unsupported(format!(
                    "Bee (PID {pid}) logs to a terminal ({tty}) — bee-tui can't read \
                     that. Restart Bee with its output redirected \
                     (`bee start … > bee.log 2>&1`), run it under systemd / docker, \
                     or launch it via `bee-tui --bee-bin`."
                ));
            }
            Some(FdTarget::DevNull) => {
                return DiscoveryResult::Unsupported(format!(
                    "Bee (PID {pid}) discards its log output (/dev/null). Restart it \
                     with output redirected to a file to see logs here."
                ));
            }
            // Opaque pipe/socket — fall through to the cgroup, which
            // is how the docker / systemd cases are identified.
            Some(FdTarget::Other(_)) => {}
            None => return DiscoveryResult::NotApplicable,
        }

        match parse_cgroup(&read_proc(pid, "cgroup")) {
            CgroupSource::Docker(id) => DiscoveryResult::Found(BeeLogSource::Command(format!(
                "docker logs -f --tail 0 {id} 2>&1"
            ))),
            CgroupSource::Systemd { unit, user } => {
                let scope = if user { "--user " } else { "" };
                DiscoveryResult::Found(BeeLogSource::Command(format!(
                    "journalctl {scope}-u {unit} -f -n 0"
                )))
            }
            CgroupSource::None => DiscoveryResult::Unsupported(format!(
                "Bee (PID {pid})'s stdout is a pipe bee-tui can't identify. Set \
                 `[[nodes]].log_command` such as `journalctl -u bee -f`, or \
                 `log_file`, to point the cockpit at the log explicitly."
            )),
        }
    }

    /// Read `/proc/<pid>/<name>`, empty string on any error.
    fn read_proc(pid: u32, name: &str) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/{name}")).unwrap_or_default()
    }

    /// Find the PID listening on TCP `port`. Collects the listening-
    /// socket inodes for the port from `/proc/net/tcp{,6}`, then
    /// scans `/proc/<pid>/fd/*` for the process owning one.
    fn find_listener_pid(port: u16) -> Option<u32> {
        let mut inodes: HashSet<u64> = HashSet::new();
        for f in ["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(content) = std::fs::read_to_string(f) {
                for line in content.lines() {
                    if let Some((p, is_listen, inode)) = parse_proc_net_line(line) {
                        if is_listen && p == port {
                            inodes.insert(inode);
                        }
                    }
                }
            }
        }
        if inodes.is_empty() {
            return None;
        }

        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(link) = std::fs::read_link(fd.path()) {
                    if let Some(inode) = link
                        .to_str()
                        .and_then(|s| s.strip_prefix("socket:["))
                        .and_then(|s| s.strip_suffix(']'))
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        if inodes.contains(&inode) {
                            return Some(pid);
                        }
                    }
                }
            }
        }
        None
    }

    /// Parse one `/proc/net/tcp{,6}` data line → `(port, is_listen,
    /// inode)`. `None` for the header line or malformed input.
    fn parse_proc_net_line(line: &str) -> Option<(u16, bool, u64)> {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            return None;
        }
        // f[1] = local_address "HEXIP:HEXPORT"; f[3] = state
        // (0A = LISTEN); f[9] = socket inode.
        let port_hex = f[1].split(':').nth(1)?;
        let port = u16::from_str_radix(port_hex, 16).ok()?;
        let is_listen = f[3] == "0A";
        let inode = f[9].parse::<u64>().ok()?;
        Some((port, is_listen, inode))
    }

    /// Classify `/proc/<pid>/fd/1` (Bee logs to stdout). `None` when
    /// the link can't be read.
    fn classify_fd_target(pid: u32) -> Option<FdTarget> {
        let target = std::fs::read_link(format!("/proc/{pid}/fd/1")).ok()?;
        let target = target.to_string_lossy().into_owned();
        Some(classify_fd_link(&target))
    }

    /// Pure classification of an fd readlink target.
    fn classify_fd_link(target: &str) -> FdTarget {
        if target == "/dev/null" {
            FdTarget::DevNull
        } else if target.starts_with("/dev/pts/")
            || target.starts_with("/dev/tty")
            || target == "/dev/console"
        {
            FdTarget::Tty(target.to_string())
        } else if target.ends_with(" (deleted)") {
            // A file that was unlinked while open — tailing the path
            // would fail or follow the wrong inode. Treat as opaque.
            FdTarget::Other(target.to_string())
        } else if target.starts_with('/') {
            FdTarget::File(PathBuf::from(target))
        } else {
            // socket:[…], pipe:[…], anon_inode:[…]
            FdTarget::Other(target.to_string())
        }
    }

    /// Parse `/proc/<pid>/cgroup` content → what's managing the
    /// process. Handles both cgroup v2 (single `0::<path>` line) and
    /// v1 (one line per controller, all carrying the same path).
    fn parse_cgroup(content: &str) -> CgroupSource {
        // Docker first — a container scope is unambiguous.
        for line in content.lines() {
            let path = line.rsplit(':').next().unwrap_or("");
            for seg in path.split('/') {
                if let Some(id) = seg
                    .strip_prefix("docker-")
                    .and_then(|s| s.strip_suffix(".scope"))
                {
                    if !id.is_empty() {
                        return CgroupSource::Docker(short_id(id));
                    }
                }
            }
            if let Some(rest) = path.split("/docker/").nth(1) {
                let id = rest.split('/').next().unwrap_or(rest);
                if !id.is_empty() {
                    return CgroupSource::Docker(short_id(id));
                }
            }
        }
        // Then a systemd `.service`: the unit is the last path
        // component ending in `.service` (skipping the intermediate
        // `user@<uid>.service` slice).
        for line in content.lines() {
            let path = line.rsplit(':').next().unwrap_or("");
            let last = path.rsplit('/').next().unwrap_or("");
            if last.ends_with(".service") && !last.starts_with("user@") {
                let user = path.contains("/user.slice/") || path.contains("user@");
                return CgroupSource::Systemd {
                    unit: last.to_string(),
                    user,
                };
            }
        }
        CgroupSource::None
    }

    /// Docker accepts a unique container-id prefix; 12 hex chars is
    /// the conventional short form.
    fn short_id(id: &str) -> String {
        id.chars().take(12).collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parse_proc_net_line_reads_listen_port_and_inode() {
            // A real LISTEN line: 127.0.0.1:1633 (0661 hex), st 0A.
            let line = "   0: 0100007F:0661 00000000:0000 0A 00000000:00000000 \
                        00:00000000 00000000  1000        0 987654 1 0000 100 0";
            let (port, is_listen, inode) = parse_proc_net_line(line).unwrap();
            assert_eq!(port, 1633);
            assert!(is_listen);
            assert_eq!(inode, 987654);
        }

        #[test]
        fn parse_proc_net_line_flags_non_listen_state() {
            // st 01 = ESTABLISHED, not LISTEN.
            let line = "   1: 0100007F:0662 0100007F:1F90 01 00000000:00000000 \
                        00:00000000 00000000  1000        0 111222 1 0000 100 0";
            let (_, is_listen, _) = parse_proc_net_line(line).unwrap();
            assert!(!is_listen);
        }

        #[test]
        fn parse_proc_net_line_rejects_header() {
            let header = "  sl  local_address rem_address   st tx_queue rx_queue \
                          tr tm->when retrnsmt   uid  timeout inode";
            assert!(parse_proc_net_line(header).is_none());
        }

        #[test]
        fn classify_fd_link_distinguishes_targets() {
            assert_eq!(classify_fd_link("/dev/null"), FdTarget::DevNull);
            assert_eq!(
                classify_fd_link("/dev/pts/7"),
                FdTarget::Tty("/dev/pts/7".into())
            );
            assert_eq!(
                classify_fd_link("/var/log/bee/bee.log"),
                FdTarget::File(PathBuf::from("/var/log/bee/bee.log"))
            );
            assert_eq!(
                classify_fd_link("socket:[12345]"),
                FdTarget::Other("socket:[12345]".into())
            );
            assert_eq!(
                classify_fd_link("pipe:[678]"),
                FdTarget::Other("pipe:[678]".into())
            );
            // A file unlinked while open is not safely tailable.
            assert_eq!(
                classify_fd_link("/tmp/bee.log (deleted)"),
                FdTarget::Other("/tmp/bee.log (deleted)".into())
            );
        }

        #[test]
        fn parse_cgroup_detects_systemd_system_unit() {
            // cgroup v2, system service.
            assert_eq!(
                parse_cgroup("0::/system.slice/bee.service\n"),
                CgroupSource::Systemd {
                    unit: "bee.service".into(),
                    user: false,
                }
            );
        }

        #[test]
        fn parse_cgroup_detects_systemd_user_unit() {
            // cgroup v2, user service — the `.service` unit is the
            // last component, and `user@`/`user.slice` marks it user.
            let c = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/bee.service\n";
            assert_eq!(
                parse_cgroup(c),
                CgroupSource::Systemd {
                    unit: "bee.service".into(),
                    user: true,
                }
            );
        }

        #[test]
        fn parse_cgroup_detects_docker_v2_and_v1() {
            let v2 = "0::/system.slice/docker-abcdef0123456789abcdef.scope\n";
            assert_eq!(
                parse_cgroup(v2),
                CgroupSource::Docker("abcdef012345".into())
            );
            let v1 = "12:pids:/docker/abcdef0123456789abcdef\n\
                      11:memory:/docker/abcdef0123456789abcdef\n";
            assert_eq!(
                parse_cgroup(v1),
                CgroupSource::Docker("abcdef012345".into())
            );
        }

        #[test]
        fn parse_cgroup_none_for_bare_shell_session() {
            // A process started in a plain shell — a session scope,
            // no managing service.
            let c = "0::/user.slice/user-1000.slice/session-3.scope\n";
            assert_eq!(parse_cgroup(c), CgroupSource::None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_parses_common_shapes() {
        assert_eq!(
            split_host_port("http://localhost:1633"),
            Some(("localhost".into(), Some(1633)))
        );
        assert_eq!(
            split_host_port("http://127.0.0.1:1633/"),
            Some(("127.0.0.1".into(), Some(1633)))
        );
        assert_eq!(
            split_host_port("https://bee.example.com"),
            Some(("bee.example.com".into(), None))
        );
        assert_eq!(
            split_host_port("http://[::1]:1633"),
            Some(("::1".into(), Some(1633)))
        );
        assert_eq!(
            split_host_port("http://user@localhost:1633"),
            Some(("localhost".into(), Some(1633)))
        );
    }

    #[test]
    fn is_local_host_recognises_loopback_only() {
        assert!(is_local_host("localhost"));
        assert!(is_local_host("127.0.0.1"));
        assert!(is_local_host("127.0.1.1"));
        assert!(is_local_host("::1"));
        assert!(is_local_host("0.0.0.0"));
        assert!(!is_local_host("192.168.1.5"));
        assert!(!is_local_host("bee.example.com"));
        assert!(!is_local_host("10.0.0.3"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn discover_is_not_applicable_off_linux() {
        assert_eq!(
            discover("http://localhost:1633"),
            DiscoveryResult::NotApplicable
        );
    }
}
