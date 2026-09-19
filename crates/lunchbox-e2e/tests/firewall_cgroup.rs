//! Does `lunchbox-firewall-helper apply-cgroup` actually filter packets?
//!
//! This is the cheap, hermetic half of the firewall's coverage: it drives the
//! helper directly against a cgroup made for the occasion, then runs the same
//! probe script the snap/flatpak tests use *inside* that cgroup and checks
//! that an allowed target connects and a denied one does not. No flatpak, no
//! snapd, no polkit, no systemd user manager, no internet — just root, cgroup
//! v2, and the BPF capabilities the CI firewall sidecar already has.
//!
//! It exists because issue #151 shipped past every other test: the embedded
//! BPF object was misaligned, so *every* `apply-cgroup` failed and every
//! firewalled flatpak ran unfiltered. `firewall_real` didn't notice because
//! the Process path uses `systemd-run` and never loads that object, and the
//! snap/flatpak tests are `#[ignore]`d behind a hand-provisioned app.
//!
//! Run as root: `sudo -E cargo test -p lunchbox-e2e --test firewall_cgroup --
//! --include-ignored --nocapture`, or via the CI firewall job. Set
//! `LUNCHBOX_FIREWALL_CGROUP_REQUIRED=1` to turn the "not applicable here"
//! skips into failures — CI sets it, so a missing precondition is reported
//! rather than passing quietly.

// Fixture code: spawns probes and stand-ins by name, which the ban makes
// deliberate rather than accidental (issue #144).
#![allow(clippy::disallowed_methods)]

use anyhow::{Context, Result, anyhow, bail};
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The uid whose cgroup subtree the helper is told to accept. Synthetic on
/// purpose: the helper only uses `PKEXEC_UID` to bound the path it will touch,
/// and borrowing a real user's `user@<uid>.service` tree would mean creating
/// and deleting cgroups underneath a live login session.
const SYNTHETIC_UID: u32 = 61000;

/// Where a denied connect is given up on. The probe script's own deny timeout
/// is 6s; a dropped SYN just retries until then.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

fn required() -> bool {
    std::env::var("LUNCHBOX_FIREWALL_CGROUP_REQUIRED").is_ok_and(|v| v != "0")
}

/// Report a precondition this host doesn't meet. A skip on a developer's
/// laptop, a failure anywhere that promised to run the test.
fn skip(reason: &str) -> Result<()> {
    if required() {
        bail!("LUNCHBOX_FIREWALL_CGROUP_REQUIRED is set but the test could not run: {reason}");
    }
    eprintln!("[SKIP] cgroup_firewall_filters_real_packets: {reason}");
    Ok(())
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/lunchbox-e2e has a repo root")
        .to_path_buf()
}

/// The helper to exercise: an explicit override, else this checkout's build,
/// else the installed copy. Deliberately prefers the build tree — the point is
/// to test what CI just compiled.
fn helper_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LUNCHBOX_FIREWALL_HELPER") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let root = repo_root();
    [
        root.join("target/debug/lunchbox-firewall-helper"),
        root.join("target/release/lunchbox-firewall-helper"),
        PathBuf::from("/usr/libexec/lunchbox-firewall-helper"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// This host's own routable IPv4, found by asking the routing table where a
/// packet to a public address would leave from. Connecting a UDP socket sends
/// nothing, so this works with no network at all — it just needs a default
/// route.
fn primary_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

/// Accept and immediately drop connections, so a probe's connect() completes.
fn serve(listener: TcpListener) {
    std::thread::spawn(move || {
        while let Ok((sock, _)) = listener.accept() {
            drop(sock);
        }
    });
}

/// The mount through which the test creates cgroups.
///
/// Normally `/sys/fs/cgroup` itself. In a container whose cgroupfs is mounted
/// read-only — which is what CI's docker-in-docker sidecar gives us — a second,
/// private cgroup2 mount of the *same* hierarchy, which is writable. Cgroups
/// made through either view are the same cgroups: the helper is always handed
/// the `/sys/fs/cgroup` path, and only ever opens it read-only.
struct CgroupWriteView {
    root: PathBuf,
    /// Set when we mounted our own view and therefore have to unmount it.
    private_mount: Option<PathBuf>,
}

impl Drop for CgroupWriteView {
    fn drop(&mut self) {
        if let Some(dir) = self.private_mount.take() {
            let _ = Command::new("umount").arg(&dir).status();
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Whether cgroups can actually be created under `root`, asked by creating one.
/// The mount flags in `/proc/self/mountinfo` are a hint, not an answer — a
/// read-write mount can still refuse in a restricted namespace.
fn can_create_cgroups_in(root: &Path) -> bool {
    let probe = root.join(format!(".lunchbox-fw-probe-{}", std::process::id()));
    match std::fs::create_dir(&probe) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&probe);
            true
        }
        Err(_) => false,
    }
}

/// How `/sys/fs/cgroup` is mounted, for the failure messages. Nothing decides
/// on this — it is what a human needs to see when the test cannot run.
fn cgroup_mount_description() -> String {
    std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .rfind(|l| l.split(' ').nth(4) == Some(CGROUP_ROOT))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "no /sys/fs/cgroup line in /proc/self/mountinfo".into())
}

fn open_write_view() -> Result<CgroupWriteView> {
    let root = PathBuf::from(CGROUP_ROOT);
    if can_create_cgroups_in(&root) {
        eprintln!("[info] creating cgroups directly under {CGROUP_ROOT}");
        return Ok(CgroupWriteView {
            root,
            private_mount: None,
        });
    }

    // Read-only view. Mounting cgroup2 again yields a writable view of the very
    // same hierarchy, so what we create here is visible under /sys/fs/cgroup
    // for the helper to open.
    let dir = tempfile::Builder::new()
        .prefix("lunchbox-cgroup2-")
        .tempdir()
        .context("temp dir for the private cgroup2 mount")?
        .keep();
    let out = Command::new("mount")
        .args(["-t", "cgroup2", "none"])
        .arg(&dir)
        .output()
        .context("run mount")?;
    if !out.status.success() {
        let _ = std::fs::remove_dir(&dir);
        bail!(
            "{CGROUP_ROOT} is not writable and mounting a private cgroup2 view failed \
             ({}): {}. Mount line: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim(),
            cgroup_mount_description()
        );
    }
    let view = CgroupWriteView {
        root: dir.clone(),
        private_mount: Some(dir.clone()),
    };
    if !can_create_cgroups_in(&view.root) {
        bail!(
            "neither {CGROUP_ROOT} nor a private cgroup2 mount at {} accepts new cgroups. \
             Mount line: {}",
            dir.display(),
            cgroup_mount_description()
        );
    }
    eprintln!(
        "[info] {CGROUP_ROOT} is not writable ({}); creating cgroups through a private \
         cgroup2 mount at {}",
        cgroup_mount_description(),
        dir.display()
    );
    Ok(view)
}

/// Create the synthetic `user@<uid>.service` subtree the helper will accept.
///
/// Returns the leaf as the test must write it (through `view`), the same leaf
/// as the helper must see it (under `/sys/fs/cgroup`), and every directory that
/// had to be created, leaf first, so the test can undo exactly what it did.
fn make_cgroup(view: &CgroupWriteView, name: &str) -> Result<(PathBuf, PathBuf, Vec<PathBuf>)> {
    let mut created = Vec::new();
    let mut write_path = view.root.clone();
    let mut helper_path = PathBuf::from(CGROUP_ROOT);
    for component in [
        "user.slice".to_string(),
        format!("user-{SYNTHETIC_UID}.slice"),
        format!("user@{SYNTHETIC_UID}.service"),
        "app.slice".to_string(),
        format!("{name}.scope"),
    ] {
        write_path.push(&component);
        helper_path.push(&component);
        if !write_path.exists() {
            std::fs::create_dir(&write_path)
                .with_context(|| format!("create cgroup {}", write_path.display()))?;
            created.push(write_path.clone());
        }
    }
    created.reverse();

    // The two views must be the same hierarchy, or the helper would be handed a
    // path to a cgroup that does not exist.
    if !helper_path.join("cgroup.procs").exists() {
        bail!(
            "cgroup created at {} is not visible at {} -- the private mount is a \
             different hierarchy",
            write_path.display(),
            helper_path.display()
        );
    }
    Ok((write_path, helper_path, created))
}

fn remove_cgroups(created: &[PathBuf]) {
    for dir in created {
        // Best effort: a cgroup with a process still in it cannot be removed,
        // and leaving one behind is harmless (the kernel detaches the BPF
        // program along with it).
        let _ = std::fs::remove_dir(dir);
    }
}

/// Run the probe script with its own pid moved into `cgroup` first, so every
/// connect it makes is subject to the program attached there.
fn run_probe_in_cgroup(cgroup: &Path, allow: &str, deny: &str, log: &Path) -> Result<()> {
    let probe = repo_root().join("scripts/integration-tests/run-firewall-probe.sh");
    if !probe.exists() {
        bail!("probe script missing at {}", probe.display());
    }

    let mut child = Command::new("bash")
        .arg("-c")
        .arg(format!(
            "echo $$ > {}/cgroup.procs && exec bash {}",
            cgroup.display(),
            probe.display()
        ))
        .env("LUNCHBOX_FIREWALL_PROBE_LOG", log)
        .env("LUNCHBOX_FIREWALL_PROBE_ALLOW", allow)
        .env("LUNCHBOX_FIREWALL_PROBE_DENY", deny)
        // The script's default is to linger for the orchestrator; nothing is
        // orchestrating here.
        .env("LUNCHBOX_FIREWALL_PROBE_HOLD_SECONDS", "0")
        .spawn()
        .context("spawn probe")?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().context("wait for probe")? {
            if !status.success() {
                bail!("probe exited with {status}");
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("probe did not finish within {PROBE_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn probe_result(log: &Path, key: &str) -> Result<String> {
    let body = std::fs::read_to_string(log)
        .with_context(|| format!("read probe log {}", log.display()))?;
    body.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("probe log has no '{key}=' line: {body:?}"))
}

/// The whole point: with `default = "deny"` and loopback allowed, a process in
/// the cgroup reaches an allowed address and cannot reach a denied one.
#[test]
#[ignore]
fn cgroup_firewall_filters_real_packets() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        return skip("must run as root (it creates cgroups and loads BPF)");
    }
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return skip("cgroup v2 is not mounted at /sys/fs/cgroup");
    }
    let Some(helper) = helper_path() else {
        return skip("lunchbox-firewall-helper not built or installed");
    };
    let Some(host_ip) = primary_ipv4() else {
        return skip("no routable non-loopback IPv4 on this host");
    };

    // Allowed: loopback, which the rules below permit. Denied: this host's own
    // routable address — off the allow list, but genuinely reachable, so a
    // BLOCKED result can only be the firewall. Nothing leaves the machine.
    let allow_listener = TcpListener::bind("127.0.0.1:0").context("bind allow listener")?;
    let allow_target = format!("127.0.0.1:{}", allow_listener.local_addr()?.port());
    let deny_listener = TcpListener::bind((host_ip, 0)).context("bind deny listener")?;
    let deny_target = format!("{host_ip}:{}", deny_listener.local_addr()?.port());
    serve(allow_listener);
    serve(deny_listener);

    // Negative control, from outside the cgroup: if the deny target were
    // unreachable anyway, "BLOCKED" below would prove nothing.
    let deny_addr: SocketAddr = deny_target.parse()?;
    std::net::TcpStream::connect_timeout(&deny_addr, Duration::from_secs(3))
        .with_context(|| format!("deny target {deny_target} must be reachable unfiltered"))?;

    // Not an error: the plain (unprivileged) CI container can neither write
    // cgroupfs nor mount cgroup2, and neither can a developer's container. That
    // is a host this test does not apply to, exactly like a missing helper —
    // and `LUNCHBOX_FIREWALL_CGROUP_REQUIRED` is what makes it fatal on the
    // hosts that promised to run it.
    let view = match open_write_view() {
        Ok(view) => view,
        Err(e) => return skip(&format!("{e:#}")),
    };
    let (write_cgroup, helper_cgroup, created) =
        make_cgroup(&view, "lunchbox-firewall-cgroup-test")?;

    let result = (|| -> Result<()> {
        let out = Command::new(&helper)
            .args([
                "apply-cgroup",
                "--cgroup-path",
                &helper_cgroup.to_string_lossy(),
                "--default",
                "deny",
                "--allow",
                "127.0.0.0/8",
                "--allow",
                "::1/128",
            ])
            .env("PKEXEC_UID", SYNTHETIC_UID.to_string())
            .output()
            .with_context(|| format!("run {}", helper.display()))?;
        if !out.status.success() {
            // Where #151 lands: the helper parses, loads, and attaches the
            // embedded BPF object here, and nothing else in CI does.
            bail!(
                "apply-cgroup failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }

        let log_dir = tempfile::tempdir().context("probe log dir")?;
        let log = log_dir.path().join("probe.log");
        // The sandbox-less probe runs as root here, but the log dir is ours.
        run_probe_in_cgroup(&write_cgroup, &allow_target, &deny_target, &log)?;

        let allow_result = probe_result(&log, "allow")?;
        let deny_result = probe_result(&log, "deny")?;
        if allow_result != "OPEN" {
            bail!("allowed target {allow_target} was {allow_result}, expected OPEN");
        }
        if deny_result != "BLOCKED" {
            bail!(
                "denied target {deny_target} was {deny_result}, expected BLOCKED -- \
                 the firewall attached but is not filtering"
            );
        }
        Ok(())
    })();

    remove_cgroups(&created);
    result?;

    // Leave a trace in the CI log that the test really ran, since its own
    // skips are otherwise silent successes.
    let mut stderr = std::io::stderr();
    let _ = writeln!(
        stderr,
        "[OK] cgroup firewall filtered as configured (allow={allow_target}, deny={deny_target})"
    );
    Ok(())
}
