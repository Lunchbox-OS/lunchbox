//! Privileged helper for shepherdd's per-activity firewall.
//!
//! Invoked by shepherdd via `pkexec`. Validates its arguments, then `exec`s
//! `systemd-run --scope` with the firewall properties and `--uid=`/`--gid=`
//! to drop privileges before the activity starts.
//!
//! See README.md for the CLI and the trust boundary.

use std::ffi::OsString;
use std::net::IpAddr;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const HELPER_NAME: &str = "shepherd-firewall-helper";

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("{}: {}", HELPER_NAME, msg.as_ref());
    std::process::exit(2);
}

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let subcmd = args
        .next()
        .map(os_to_string)
        .unwrap_or_else(|| die("missing subcommand (expected 'apply-process' or 'stop-scope')"));

    match subcmd.as_str() {
        "apply-process" => apply_process(args),
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
    let pkexec_uid = std::env::var("PKEXEC_UID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or_else(|| die("PKEXEC_UID is unset; this helper must be invoked via pkexec"));
    if uid != pkexec_uid {
        die(format!(
            "--uid {} does not match PKEXEC_UID {}",
            uid, pkexec_uid
        ));
    }

    if command_argv.is_empty() {
        die("missing command after '--'");
    }
    // Allow either an absolute path that exists, or a bare command name
    // (alnum + `_-.`) which systemd-run will resolve via PATH. This isn't a
    // privilege boundary -- the activity runs as PKEXEC_UID (the calling
    // user, who could spawn anything as themselves anyway) -- but it keeps
    // the helper's argv tidy and rejects obvious tampering.
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

    let err = Command::new("systemd-run").args(&sd_args).exec();
    eprintln!("{}: execvp(systemd-run): {}", HELPER_NAME, err);
    ExitCode::from(127)
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

    let err = Command::new("systemctl").args(["stop", &scope_name]).exec();
    eprintln!("{}: execvp(systemctl): {}", HELPER_NAME, err);
    ExitCode::from(127)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

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
        assert!(is_valid_scope_name("shepherd-abc.scope"));
        assert!(is_valid_scope_name("shepherd-1234567890abcdef.scope"));
        assert!(!is_valid_scope_name("shepherd-abc"));
        assert!(!is_valid_scope_name("shepherd abc.scope"));
        assert!(!is_valid_scope_name(".scope"));
        assert!(!is_valid_scope_name("../shepherd.scope"));
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
}
