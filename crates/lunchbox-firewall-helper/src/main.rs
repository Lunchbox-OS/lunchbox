//! Privileged helper for lunchboxd's per-activity firewall.
//!
//! Invoked by lunchboxd via `pkexec`. Three subcommands:
//!
//! - `apply-process`: legacy/Process-kind. Validates argv, then `exec`s
//!   `systemd-run --scope --uid=… --property=IPAddress*=…` so systemd
//!   creates a fresh transient scope with the BPF address filter attached.
//! - `apply-cgroup`: Snap/Flatpak. The runtime already created the scope;
//!   we open the cgroup and attach our own `cgroup_skb/egress` BPF program
//!   to it. The program is compiled from the sibling `lunchbox-firewall-bpf`
//!   crate and embedded via `include_bytes!`.
//! - `stop-scope`: shells out to `systemctl stop <unit>`.
//!
//! See README.md for the trust boundary.

use std::ffi::OsString;
use std::net::IpAddr;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

mod bpf;

const HELPER_NAME: &str = "lunchbox-firewall-helper";

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{}: {}", HELPER_NAME, msg.as_ref());
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let subcmd = args.next().map(os_to_string).unwrap_or_else(|| {
        die("missing subcommand (expected 'apply-process', 'apply-cgroup', or 'stop-scope')")
    });

    match subcmd.as_str() {
        "apply-process" => apply_process(args),
        "apply-cgroup" => apply_cgroup(args),
        "stop-scope" => stop_scope(args),
        other => die(format!("unknown subcommand '{}'", other)),
    }
}

fn os_to_string(s: OsString) -> String {
    s.into_string()
        .unwrap_or_else(|_| die("argument is not valid UTF-8"))
}

// ---------------------------------------------------------------------------
// apply-process
// ---------------------------------------------------------------------------

fn apply_process(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut uid: Option<u32> = None;
    let mut gid: Option<u32> = None;
    let mut scope_name: Option<String> = None;
    let mut default_deny: Option<bool> = None;
    let mut allow_rules: Vec<String> = Vec::new();
    let mut deny_rules: Vec<String> = Vec::new();
    let mut env_pairs: Vec<(String, String)> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut command_argv: Vec<String> = Vec::new();

    let mut iter = args.map(os_to_string);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--uid" => {
                uid = Some(parse_uid(
                    &iter.next().unwrap_or_else(|| die("--uid needs value")),
                ))
            }
            "--gid" => {
                gid = Some(parse_uid(
                    &iter.next().unwrap_or_else(|| die("--gid needs value")),
                ))
            }
            "--scope-name" => {
                let v = iter
                    .next()
                    .unwrap_or_else(|| die("--scope-name needs value"));
                if !is_valid_scope_name(&v) {
                    die(format!("invalid scope name '{}'", v));
                }
                scope_name = Some(v);
            }
            "--default" => {
                let v = iter.next().unwrap_or_else(|| die("--default needs value"));
                default_deny = Some(match v.as_str() {
                    "deny" => true,
                    "allow" => false,
                    _ => die("--default must be 'deny' or 'allow'"),
                });
            }
            "--allow" => {
                let v = iter.next().unwrap_or_else(|| die("--allow needs value"));
                if !is_valid_rule(&v) {
                    die(format!("invalid --allow value '{}'", v));
                }
                allow_rules.push(v);
            }
            "--deny" => {
                let v = iter.next().unwrap_or_else(|| die("--deny needs value"));
                if !is_valid_rule(&v) {
                    die(format!("invalid --deny value '{}'", v));
                }
                deny_rules.push(v);
            }
            "--env" => {
                let v = iter.next().unwrap_or_else(|| die("--env needs value"));
                let (k, val) = match v.split_once('=') {
                    Some((k, val)) => (k.to_string(), val.to_string()),
                    None => die(format!("--env needs KEY=VALUE, got '{}'", v)),
                };
                if !is_valid_env_key(&k) {
                    die(format!("invalid env key '{}'", k));
                }
                env_pairs.push((k, val));
            }
            "--cwd" => {
                let v = iter.next().unwrap_or_else(|| die("--cwd needs value"));
                if !is_valid_cwd(&v) {
                    die(format!("invalid --cwd '{}'", v));
                }
                cwd = Some(v);
            }
            "--" => {
                command_argv.extend(iter);
                break;
            }
            other => die(format!("unknown option '{}'", other)),
        }
    }

    let uid = uid.unwrap_or_else(|| die("--uid is required"));
    let gid = gid.unwrap_or_else(|| die("--gid is required"));
    let scope_name = scope_name.unwrap_or_else(|| die("--scope-name is required"));
    let default_deny = default_deny.unwrap_or_else(|| die("--default is required"));

    if uid == 0 {
        die("--uid 0 is not allowed");
    }

    // Bind --uid to PKEXEC_UID: a user granted the action must not be able to
    // launch a process as a different user.
    let pkexec_uid = require_pkexec_uid();
    if uid != pkexec_uid {
        die(format!(
            "--uid {} does not match PKEXEC_UID {}",
            uid, pkexec_uid
        ));
    }

    if command_argv.is_empty() {
        die("missing command after '--'");
    }
    if !is_acceptable_command(&command_argv[0]) {
        die(format!(
            "command must be an absolute path or a bare name (alnum + '_-.') , got '{}'",
            command_argv[0]
        ));
    }

    // Build the systemd-run argv. We use --scope (system manager), which has
    // the kernel privileges needed to attach the cgroup_skb BPF programs that
    // back IPAddressDeny=/IPAddressAllow=.
    let mut sd_args: Vec<String> = vec![
        "--scope".into(),
        "--collect".into(),
        "--quiet".into(),
        format!("--unit={}", scope_name),
        format!("--uid={}", uid),
        format!("--gid={}", gid),
    ];
    // Make the scope die with the session that asked for it (issue #172).
    sd_args.extend(lifetime_args(uid, session_scope_of_self().as_deref()));
    if let Some(c) = cwd {
        sd_args.push(format!("--working-directory={}", c));
    }
    if default_deny {
        sd_args.push("--property=IPAddressDeny=any".into());
    }
    for r in &allow_rules {
        sd_args.push(format!("--property=IPAddressAllow={}", r));
    }
    for r in &deny_rules {
        sd_args.push(format!("--property=IPAddressDeny={}", r));
    }
    for (k, v) in &env_pairs {
        sd_args.push(format!("--setenv={}={}", k, v));
    }
    sd_args.push("--".into());
    sd_args.extend(command_argv);

    // Bare names are safe here, unlike anywhere in the daemon (issue #144).
    // This binary only ever runs under `pkexec`, which replaces the environment
    // with "a minimal known and safe" one — measured: `PATH` comes through as
    // `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:...`, all
    // root-owned. The caller's `$PATH` never reaches here, which is the whole
    // reason the helper is reached through `pkexec` rather than called directly.
    #[allow(clippy::disallowed_methods)]
    let err = Command::new("systemd-run").args(&sd_args).exec();
    eprintln!("{}: execvp(systemd-run): {}", HELPER_NAME, err);
    ExitCode::from(127)
}

// ---------------------------------------------------------------------------
// apply-cgroup
// ---------------------------------------------------------------------------

/// `apply-cgroup --cgroup-path P --default deny|allow [--allow R]... [--deny R]...`
///
/// Loads the embedded cgroup_skb BPF program, populates its rule maps from
/// the argv, then attaches `cgroup_skb/egress` to the cgroup at `P` via the
/// legacy `BPF_PROG_ATTACH` syscall (so the attach persists after the
/// helper exits). The kernel detaches the program automatically when the
/// cgroup is destroyed.
fn apply_cgroup(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut cgroup_path: Option<String> = None;
    let mut default_deny: Option<bool> = None;
    let mut allow_rules: Vec<String> = Vec::new();
    let mut deny_rules: Vec<String> = Vec::new();

    let mut iter = args.map(os_to_string);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--cgroup-path" => {
                let v = iter
                    .next()
                    .unwrap_or_else(|| die("--cgroup-path needs value"));
                cgroup_path = Some(v);
            }
            "--default" => {
                let v = iter.next().unwrap_or_else(|| die("--default needs value"));
                default_deny = Some(match v.as_str() {
                    "deny" => true,
                    "allow" => false,
                    _ => die("--default must be 'deny' or 'allow'"),
                });
            }
            "--allow" => {
                let v = iter.next().unwrap_or_else(|| die("--allow needs value"));
                if !is_valid_rule(&v) {
                    die(format!("invalid --allow value '{}'", v));
                }
                allow_rules.push(v);
            }
            "--deny" => {
                let v = iter.next().unwrap_or_else(|| die("--deny needs value"));
                if !is_valid_rule(&v) {
                    die(format!("invalid --deny value '{}'", v));
                }
                deny_rules.push(v);
            }
            other => die(format!("unknown option '{}'", other)),
        }
    }

    let cgroup_path = cgroup_path.unwrap_or_else(|| die("--cgroup-path is required"));
    let default_deny = default_deny.unwrap_or_else(|| die("--default is required"));

    let pkexec_uid = require_pkexec_uid();
    if !is_valid_user_cgroup_path(&cgroup_path, pkexec_uid) {
        die(format!(
            "--cgroup-path must be an absolute path under \
             /sys/fs/cgroup/user.slice/user-{0}.slice/user@{0}.service/, got '{1}'",
            pkexec_uid, cgroup_path
        ));
    }

    match bpf::apply_cgroup(&cgroup_path, default_deny, &allow_rules, &deny_rules) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}: apply-cgroup failed: {:#}", HELPER_NAME, e);
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// stop-scope
// ---------------------------------------------------------------------------

fn stop_scope(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut scope_name: Option<String> = None;
    let mut iter = args.map(os_to_string);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--scope-name" => {
                let v = iter
                    .next()
                    .unwrap_or_else(|| die("--scope-name needs value"));
                if !is_valid_scope_name(&v) {
                    die(format!("invalid scope name '{}'", v));
                }
                scope_name = Some(v);
            }
            other => die(format!("unknown option '{}'", other)),
        }
    }
    let scope_name = scope_name.unwrap_or_else(|| die("--scope-name is required"));

    // Same as `apply_process`: pkexec has already replaced the environment.
    #[allow(clippy::disallowed_methods)]
    let err = Command::new("systemctl").args(["stop", &scope_name]).exec();
    eprintln!("{}: execvp(systemctl): {}", HELPER_NAME, err);
    ExitCode::from(127)
}

// ---------------------------------------------------------------------------
// Lifetime (issue #172)
// ---------------------------------------------------------------------------

/// The properties that tie this scope's life to the session that asked for it.
///
/// Without them a firewalled activity outlives everything. The scope is a
/// **system** manager unit -- it has to be, because the `cgroup_skb` programs
/// behind `IPAddressDeny=` need `CAP_NET_ADMIN` and a per-user manager cannot
/// attach them -- so it sits outside `user-<uid>.slice`, where neither logind's
/// `TerminateSession`, nor its teardown of `user@<uid>.service`, nor `KillUser`
/// reaches it. The only thing that ever stopped one was `lunchboxd` itself
/// calling back through this helper's `stop-scope`, and issue #172 is precisely
/// about `lunchboxd` not being there to call anything.
///
/// So the lifetime is declared when the scope is created rather than enforced
/// when something dies. Nothing gains an authority it did not have: the session
/// watchdog's existing `TerminateSession` ends up doing the whole job.
///
/// Two properties, because they answer different questions:
///
/// * `--slice=user-<uid>.slice` puts the scope in the kiosk user's slice, which
///   logind stops when the user's last session goes -- and which is also what
///   `KillUser` kills, so the watchdog's escalation reaches it too. A unit gets
///   an implicit `Requires=` on its slice, and a stopped slice stops what
///   requires it.
/// * `BindsTo=`/`After=` the caller's own session scope ends it when *that
///   session* ends, which is the sharper statement: a device with two kiosk
///   users has two sessions and one user slice each, and an activity belongs to
///   a session rather than to a uid.
///
/// The session is **derived, never passed**. The polkit rule admits the
/// `lunchbox-firewall` group, which is the kiosk user, so an argument here
/// would be attacker-chosen. Reading it from this process's own cgroup -- which
/// is the caller's, inherited through `pkexec`, before `systemd-run` moves
/// anything -- means a caller can only ever name the session it is actually in.
/// (Lying would in any case only shorten its own activity's life: `BindsTo` is
/// one-way and cannot stop the unit it names.)
///
/// When the caller is not in a session scope at all -- a development stack, a
/// hand-run helper -- the binding is simply omitted rather than guessed, and
/// the slice still applies.
fn lifetime_args(uid: u32, session_scope: Option<&str>) -> Vec<String> {
    let mut args = vec![format!("--slice=user-{}.slice", uid)];
    if let Some(scope) = session_scope {
        args.push(format!("--property=BindsTo={}", scope));
        args.push(format!("--property=After={}", scope));
    }
    args
}

/// The session scope this process is in, if it is in one.
fn session_scope_of_self() -> Option<String> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    session_scope_in_cgroup(&cgroup)
}

/// Pick `session-<id>.scope` out of the contents of `/proc/<pid>/cgroup`.
///
/// Validated rather than trusted even though it comes from the kernel: it goes
/// into an argv, so it is held to logind's own shape -- `session-`, an
/// alphanumeric id, `.scope` -- and anything else is treated as "not in a
/// session".
fn session_scope_in_cgroup(cgroup: &str) -> Option<String> {
    cgroup
        .lines()
        .flat_map(|line| line.rsplit('/'))
        .find(|component| is_session_scope(component))
        .map(str::to_string)
}

fn is_session_scope(component: &str) -> bool {
    let Some(id) = component
        .strip_prefix("session-")
        .and_then(|rest| rest.strip_suffix(".scope"))
    else {
        return false;
    };
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn require_pkexec_uid() -> u32 {
    std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or_else(|| die("PKEXEC_UID is unset; this helper must be invoked via pkexec"))
}

fn parse_uid(s: &str) -> u32 {
    s.parse::<u32>()
        .unwrap_or_else(|_| die(format!("'{}' is not a valid uid/gid", s)))
}

/// Strict allowlist for firewall rules:
/// * the four systemd address tokens
/// * literal IPv4/IPv6 addresses, optionally with CIDR prefix length
fn is_valid_rule(s: &str) -> bool {
    matches!(s, "any" | "localhost" | "link-local" | "multicast") || is_ip_or_cidr(s)
}

fn is_ip_or_cidr(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_hexdigit() || b == b'.' || b == b':' || b == b'/')
    {
        return false;
    }
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip = match addr.parse::<IpAddr>() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    if let Some(p) = prefix {
        let prefix: u32 = match p.parse() {
            Ok(n) => n,
            Err(_) => return false,
        };
        let max = if ip.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return false;
        }
    }
    true
}

/// systemd unit names: alnum, `:_-.\@`. We additionally require it ends in
/// `.scope` (since `--scope` requires the unit name to have that suffix when
/// `--unit=` is given).
fn is_valid_scope_name(s: &str) -> bool {
    if !s.ends_with(".scope") {
        return false;
    }
    let stem = &s[..s.len() - ".scope".len()];
    if stem.is_empty() {
        return false;
    }
    stem.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'@' | b'\\'))
}

fn is_valid_env_key(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.as_bytes()[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn is_valid_cwd(s: &str) -> bool {
    s.starts_with('/') && !s.contains('\0')
}

fn is_acceptable_command(s: &str) -> bool {
    if s.is_empty() || s.contains('\0') || s.contains('\n') {
        return false;
    }
    if s.starts_with('/') {
        return std::fs::metadata(s).map(|m| m.is_file()).unwrap_or(false);
    }
    // Bare name: alnum + _ - . only.
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Cgroup path that may legitimately host an activity for `pkexec_uid`.
/// Must:
/// * be absolute
/// * be under `/sys/fs/cgroup/user.slice/user-<uid>.slice/user@<uid>.service/`
/// * contain only safe characters (no `..`, no `\0`, no `\n`)
/// * resolve to an existing directory containing `cgroup.procs`
fn is_valid_user_cgroup_path(path: &str, pkexec_uid: u32) -> bool {
    let prefix = format!(
        "/sys/fs/cgroup/user.slice/user-{0}.slice/user@{0}.service/",
        pkexec_uid
    );
    if !path.starts_with(&prefix) {
        return false;
    }
    if path.contains('\0') || path.contains('\n') {
        return false;
    }
    // Reject "..": the suffix path must not be able to escape the user@
    // subtree.
    for component in path.split('/').skip(1) {
        if component == ".." {
            return false;
        }
    }
    // The cgroup directory exists and contains cgroup.procs.
    let p = std::path::Path::new(path);
    let m = match std::fs::metadata(p) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !m.is_dir() {
        return false;
    }
    p.join("cgroup.procs").exists()
}

// ---------------------------------------------------------------------------
// Tests (validation logic only -- exec paths require a running systemd)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_accepts_tokens_and_cidrs() {
        for ok in [
            "any",
            "localhost",
            "link-local",
            "multicast",
            "127.0.0.1",
            "10.0.0.0/8",
            "::1",
            "::1/128",
            "2001:db8::/32",
        ] {
            assert!(is_valid_rule(ok), "expected {} to be valid", ok);
        }
    }

    #[test]
    fn rule_rejects_garbage() {
        for bad in [
            "",
            "google.com",
            "10.0.0.1; rm -rf /",
            "$(echo)",
            "10.0.0.1/33",
            "::1/129",
            "10.0.0.1\n",
        ] {
            assert!(!is_valid_rule(bad), "expected {} to be rejected", bad);
        }
    }

    #[test]
    fn env_key_is_pos_validated() {
        assert!(is_valid_env_key("PATH"));
        assert!(is_valid_env_key("WAYLAND_DISPLAY"));
        assert!(is_valid_env_key("_LEADING_UNDERSCORE"));
        assert!(!is_valid_env_key(""));
        assert!(!is_valid_env_key("1NUMERIC_LEAD"));
        assert!(!is_valid_env_key("HAS-DASH"));
        assert!(!is_valid_env_key("HAS SPACE"));
    }

    #[test]
    fn scope_name_must_end_in_scope() {
        assert!(is_valid_scope_name("lunchbox-abc.scope"));
        assert!(is_valid_scope_name("lunchbox-1234567890abcdef.scope"));
        assert!(!is_valid_scope_name("lunchbox-abc"));
        assert!(!is_valid_scope_name("lunchbox abc.scope"));
        assert!(!is_valid_scope_name(".scope"));
        assert!(!is_valid_scope_name("../lunchbox.scope"));
    }

    #[test]
    fn cwd_requires_absolute() {
        assert!(is_valid_cwd("/home/user"));
        assert!(!is_valid_cwd("relative"));
        assert!(!is_valid_cwd(""));
    }

    #[test]
    fn command_accepts_bare_names_and_existing_absolutes() {
        assert!(is_acceptable_command("ptyxis"));
        assert!(is_acceptable_command("scummvm-1.0"));
        assert!(is_acceptable_command("/bin/sh"));
        assert!(!is_acceptable_command(""));
        assert!(!is_acceptable_command("foo bar"));
        assert!(!is_acceptable_command("foo;bar"));
        assert!(!is_acceptable_command("/this/path/should/not/exist"));
        assert!(!is_acceptable_command("relative/path"));
    }

    #[test]
    fn a_firewalled_scope_is_bound_to_the_session_that_asked_for_it() {
        // Issue #172. Without these the scope is a system-manager unit outside
        // the user's slice, which nothing ends when the session does -- so a
        // killed lunchboxd leaves a firewalled activity running with its rules
        // and no supervisor.
        let args = lifetime_args(1000, Some("session-2.scope"));
        assert!(args.contains(&"--slice=user-1000.slice".to_string()));
        assert!(args.contains(&"--property=BindsTo=session-2.scope".to_string()));
        assert!(
            args.contains(&"--property=After=session-2.scope".to_string()),
            "BindsTo without After orders the teardown by luck"
        );
    }

    #[test]
    fn no_session_means_the_slice_alone_rather_than_a_guess() {
        // A development stack, or the helper run by hand: there is no session
        // scope to bind to, and naming one that does not exist would fail the
        // unit's start -- which is an activity that will not launch, for a
        // property that is defence in depth.
        let args = lifetime_args(1000, None);
        assert_eq!(args, vec!["--slice=user-1000.slice".to_string()]);
    }

    #[test]
    fn the_session_comes_out_of_a_real_cgroup_line() {
        // v2, which is what a device has.
        assert_eq!(
            session_scope_in_cgroup("0::/user.slice/user-1000.slice/session-2.scope\n"),
            Some("session-2.scope".to_string())
        );
        // The activity's own scope is deeper, but this helper runs before
        // `systemd-run` moves anything, so what it sees is the session.
        assert_eq!(
            session_scope_in_cgroup("0::/user.slice/user-1000.slice/session-c1.scope\n"),
            Some("session-c1.scope".to_string())
        );
    }

    #[test]
    fn anything_that_is_not_a_session_scope_is_no_session() {
        // The value reaches an argv, so it is held to logind's shape rather
        // than trusted for coming from the kernel.
        for cgroup in [
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/lunchbox-abc.scope\n",
            "0::/system.slice/lunchbox-stated@kiosk.service\n",
            "0::/user.slice/user-1000.slice/session-.scope\n",
            "0::/user.slice/user-1000.slice/session-2;rm.scope\n",
            "0::/\n",
            "",
        ] {
            assert_eq!(
                session_scope_in_cgroup(cgroup),
                None,
                "should not have read a session out of {cgroup:?}"
            );
        }
    }

    #[test]
    fn cgroup_path_must_be_under_user_subtree() {
        // We can only test the structural check, not the existence one (that
        // requires a real cgroup).
        let uid = 1000;
        // Wrong prefixes are always rejected.
        assert!(!is_valid_user_cgroup_path("relative", uid));
        assert!(!is_valid_user_cgroup_path("/", uid));
        assert!(!is_valid_user_cgroup_path(
            "/sys/fs/cgroup/system.slice/foo.scope",
            uid
        ));
        assert!(!is_valid_user_cgroup_path(
            "/sys/fs/cgroup/user.slice/user-2000.slice/user@2000.service/foo.scope",
            uid
        ));
        // `..` blocked.
        assert!(!is_valid_user_cgroup_path(
            "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/../foo",
            uid
        ));
    }
}
