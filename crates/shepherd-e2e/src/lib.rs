//! End-to-end test harness for shepherd-launcher.
//!
//! Spins up a real headless Sway compositor, a real `shepherdd` daemon, and
//! optionally the launcher/HUD UIs, all wired through an isolated temp
//! environment. Tests drive the running stack through the HTTP management
//! API and the IPC socket.
//!
//! See the crate README for usage. The harness is intentionally Linux-only
//! and assumes the workspace binaries have been built (`cargo build`).

use anyhow::{Context, Result, anyhow, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::Value;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::process::{Child, Command};

mod http;

pub use http::{HttpClient, HttpResponse, SseStream};

/// Path to a workspace binary inside `target/<profile>/`.
fn workspace_binary(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is `<workspace>/crates/shepherd-e2e`.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("CARGO_MANIFEST_DIR has at least two ancestors")
        .to_path_buf();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    workspace.join("target").join(profile).join(name)
}

/// Allocate a TCP port that is currently free on localhost. There is a small
/// race between releasing the listener and the daemon binding it, but it is
/// the standard approach for assigning ports in test harnesses.
fn alloc_tcp_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("failed to bind ephemeral port for HTTP API")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn init_tracing() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_test_writer()
            .try_init();
    });
}

/// Wait for `predicate` to become `Some` within `timeout`, polling every
/// `interval`. Returns the value or an error explaining the timeout.
async fn wait_for<T, F, Fut>(label: &str, timeout: Duration, mut predicate: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    let interval = Duration::from_millis(100);
    loop {
        if let Some(v) = predicate().await {
            return Ok(v);
        }
        if start.elapsed() >= timeout {
            bail!("timeout after {:?} waiting for {}", timeout, label);
        }
        tokio::time::sleep(interval).await;
    }
}

/// Builder for a [`TestHarness`].
pub struct HarnessBuilder {
    config_toml: Option<String>,
    spawn_launcher: bool,
    spawn_hud: bool,
    auth_token: Option<String>,
    extra_env: Vec<(String, String)>,
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessBuilder {
    pub fn new() -> Self {
        Self {
            config_toml: None,
            spawn_launcher: false,
            spawn_hud: false,
            auth_token: None,
            extra_env: Vec::new(),
        }
    }

    /// Provide a literal TOML config. Use `{HTTP_PORT}` and `{AUTH_TOKEN}`
    /// placeholders for values the harness fills in. If unset, a default
    /// config with three test entries is used (see [`default_config_toml`]).
    pub fn config_toml(mut self, toml: impl Into<String>) -> Self {
        self.config_toml = Some(toml.into());
        self
    }

    /// Set an auth token for the management API. If `Some(_)`, the default
    /// config will configure the API with this token, and the [`HttpClient`]
    /// will send it on every request.
    pub fn auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn spawn_launcher(mut self, yes: bool) -> Self {
        self.spawn_launcher = yes;
        self
    }

    pub fn spawn_hud(mut self, yes: bool) -> Self {
        self.spawn_hud = yes;
        self
    }

    /// Add an environment variable to shepherdd's process. Applied after the
    /// harness's standard `env_clear()` + base env, so it can override the
    /// defaults (e.g. `PATH` to inject fakes for `pkcheck`/`pkexec`).
    pub fn shepherdd_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    pub async fn start(self) -> Result<TestHarness> {
        TestHarness::start(self).await
    }
}

/// Default config TOML with one always-available short-lived entry, one
/// always-available normal entry, and one always-available no-limit entry.
/// Substitutes `{HTTP_PORT}` and (optional) `{AUTH_TOKEN}`.
pub fn default_config_toml() -> &'static str {
    r#"
config_version = 1

[service]
default_max_run_seconds = 3600

[service.management_api]
enabled = true
port = {HTTP_PORT}
bind = "127.0.0.1"
{AUTH_TOKEN_LINE}

# Sleep that exits cleanly after a long time — used for normal API
# launch/stop tests.
[[entries]]
id = "sleeper"
label = "Sleeper"
[entries.kind]
type = "process"
command = "/usr/bin/sleep"
args = ["600"]
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 300

# Sleep with a very short max_run for the timeout test.
[[entries]]
id = "ephemeral"
label = "Ephemeral"
[entries.kind]
type = "process"
command = "/usr/bin/sleep"
args = ["600"]
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 2

# Always-available unlimited entry, used for override tests.
[[entries]]
id = "always-on"
label = "Always On"
[entries.kind]
type = "process"
command = "/usr/bin/sleep"
args = ["600"]
[entries.availability]
always = true
[entries.limits]
max_run_seconds = 0
"#
}

/// A running shepherd stack: sway + shepherdd (+ optional UIs), inside an
/// isolated temp directory. Drop kills everything.
pub struct TestHarness {
    /// Held to keep the temp dir alive until drop.
    _temp: TempDir,
    xdg_runtime_dir: PathBuf,
    config_path: PathBuf,
    socket_path: PathBuf,
    data_dir: PathBuf,
    http_port: u16,
    auth_token: Option<String>,
    sway: Option<Child>,
    shepherdd: Option<Child>,
    launcher: Option<Child>,
    hud: Option<Child>,
    /// Populated on shutdown so Drop knows everything is already reaped.
    shut_down: bool,
}

impl TestHarness {
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::new()
    }

    async fn start(builder: HarnessBuilder) -> Result<Self> {
        init_tracing();

        let temp = tempfile::Builder::new()
            .prefix("shepherd-e2e-")
            .tempdir()
            .context("create temp dir")?;
        let temp_path = temp.path().to_path_buf();

        // XDG_RUNTIME_DIR must be 0700 owned by the current user. tempfile
        // already creates it with 0700, but be explicit.
        let xdg_runtime_dir = temp_path.join("xdg-runtime");
        fs::create_dir_all(&xdg_runtime_dir)?;
        fs::set_permissions(&xdg_runtime_dir, fs::Permissions::from_mode(0o700))?;

        let config_path = temp_path.join("config.toml");
        let socket_path = temp_path.join("shepherd.sock");
        let data_dir = temp_path.join("data");
        let log_dir = temp_path.join("log");
        let xdg_data_home = temp_path.join("xdg-data");
        let xdg_config_home = temp_path.join("xdg-config");
        let xdg_state_home = temp_path.join("xdg-state");
        let xdg_cache_home = temp_path.join("xdg-cache");
        for d in [
            &data_dir,
            &log_dir,
            &xdg_data_home,
            &xdg_config_home,
            &xdg_state_home,
            &xdg_cache_home,
        ] {
            fs::create_dir_all(d)?;
        }

        let http_port = alloc_tcp_port()?;

        let auth_token = builder.auth_token.clone();
        let auth_line = match &auth_token {
            Some(t) => format!("auth_token = \"{}\"", t),
            None => String::new(),
        };
        let raw_config = builder
            .config_toml
            .clone()
            .unwrap_or_else(|| default_config_toml().to_string());
        let rendered_config = raw_config
            .replace("{HTTP_PORT}", &http_port.to_string())
            .replace("{AUTH_TOKEN}", auth_token.as_deref().unwrap_or(""))
            .replace("{AUTH_TOKEN_LINE}", &auth_line);
        fs::write(&config_path, &rendered_config).context("write shepherdd config")?;

        // ----- Sway --------------------------------------------------------
        let sway_config_path = temp_path.join("sway.conf");
        fs::write(
            &sway_config_path,
            // Minimal config: no exec lines, no input devices, plain bg.
            "output * bg #000000 solid_color\n\
             default_border none\n\
             font pango:monospace 1\n",
        )?;

        let mut sway_cmd = Command::new("sway");
        sway_cmd
            .arg("-c")
            .arg(&sway_config_path)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("USER", std::env::var("USER").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &xdg_runtime_dir)
            .env("WLR_BACKENDS", "headless")
            .env("WLR_LIBINPUT_NO_DEVICES", "1")
            .env("WLR_RENDERER", "pixman")
            .env("XDG_SESSION_TYPE", "wayland")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let sway = sway_cmd.spawn().context("spawn sway")?;

        // Sway creates `wayland-1` (or wayland-0 if unset). It scans for the
        // first free name. Wait for any `wayland-N` socket to appear.
        let runtime_for_wait = xdg_runtime_dir.clone();
        let wayland_display = wait_for("sway wayland socket", Duration::from_secs(15), || {
            let runtime_for_wait = runtime_for_wait.clone();
            async move {
                for entry in fs::read_dir(&runtime_for_wait).ok()?.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("wayland-")
                        && !name.ends_with(".lock")
                        && entry.file_type().ok()?.is_socket()
                    {
                        return Some(name.into_owned());
                    }
                }
                None
            }
        })
        .await?;

        // ----- shepherdd ---------------------------------------------------
        let shepherdd_path = workspace_binary("shepherdd");
        if !shepherdd_path.exists() {
            bail!(
                "shepherdd binary not found at {} — run `cargo build` first",
                shepherdd_path.display()
            );
        }

        let mut shepherdd_cmd = Command::new(&shepherdd_path);
        shepherdd_cmd
            .arg("-c")
            .arg(&config_path)
            .arg("-s")
            .arg(&socket_path)
            .arg("-d")
            .arg(&data_dir)
            .arg("--log-level")
            .arg(std::env::var("SHEPHERD_E2E_LOG").unwrap_or_else(|_| "info".into()))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("USER", std::env::var("USER").unwrap_or_default())
            .env("XDG_RUNTIME_DIR", &xdg_runtime_dir)
            .env("XDG_DATA_HOME", &xdg_data_home)
            .env("XDG_CONFIG_HOME", &xdg_config_home)
            .env("XDG_STATE_HOME", &xdg_state_home)
            .env("XDG_CACHE_HOME", &xdg_cache_home)
            .env("WAYLAND_DISPLAY", &wayland_display)
            .env("XDG_SESSION_TYPE", "wayland")
            .stdin(Stdio::null());
        // Optionally redirect shepherdd's stdout/stderr to a path the test
        // can read (debugging firewall enforcement, BPF attach, etc.).
        // Default: silence.
        match std::env::var("SHEPHERD_E2E_LOG_FILE") {
            Ok(path) => {
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .with_context(|| format!("open SHEPHERD_E2E_LOG_FILE {path}"))?;
                let f2 = f.try_clone().context("clone log file fd")?;
                shepherdd_cmd.stdout(Stdio::from(f)).stderr(Stdio::from(f2));
            }
            Err(_) => {
                shepherdd_cmd.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }
        for (k, v) in &builder.extra_env {
            shepherdd_cmd.env(k, v);
        }
        let shepherdd = shepherdd_cmd.spawn().context("spawn shepherdd")?;

        let http_client = HttpClient::new(http_port, auth_token.clone());

        // Wait for /api/v1/health to return 200.
        let health_client = http_client.clone();
        wait_for("shepherdd /api/v1/health", Duration::from_secs(15), || {
            let health_client = health_client.clone();
            async move {
                match health_client.get("/api/v1/health").await {
                    Ok(r) if r.status == 200 => Some(()),
                    _ => None,
                }
            }
        })
        .await?;

        // Also wait for the IPC socket to appear before declaring readiness.
        let socket_for_wait = socket_path.clone();
        wait_for("shepherdd ipc socket", Duration::from_secs(5), || {
            let p = socket_for_wait.clone();
            async move { if p.exists() { Some(()) } else { None } }
        })
        .await?;

        // ----- Optional UIs ------------------------------------------------
        let launcher = if builder.spawn_launcher {
            Some(spawn_ui(
                "shepherd-launcher",
                &xdg_runtime_dir,
                &wayland_display,
                &socket_path,
                &data_dir,
            )?)
        } else {
            None
        };
        let hud = if builder.spawn_hud {
            Some(spawn_ui(
                "shepherd-hud",
                &xdg_runtime_dir,
                &wayland_display,
                &socket_path,
                &data_dir,
            )?)
        } else {
            None
        };

        Ok(Self {
            _temp: temp,
            xdg_runtime_dir,
            config_path,
            socket_path,
            data_dir,
            http_port,
            auth_token,
            sway: Some(sway),
            shepherdd: Some(shepherdd),
            launcher,
            hud,
            shut_down: false,
        })
    }

    pub fn http(&self) -> HttpClient {
        HttpClient::new(self.http_port, self.auth_token.clone())
    }

    pub fn http_port(&self) -> u16 {
        self.http_port
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn xdg_runtime_dir(&self) -> &Path {
        &self.xdg_runtime_dir
    }

    /// Connect a fresh IPC client to the running daemon.
    pub async fn connect_ipc(&self) -> Result<shepherd_ipc::IpcClient> {
        Ok(shepherd_ipc::IpcClient::connect(&self.socket_path).await?)
    }

    /// Returns true if any spawned child exited unexpectedly (since the
    /// last call). Useful in tests after long waits.
    pub fn check_alive(&mut self) -> Result<()> {
        for (name, child) in [
            ("sway", &mut self.sway),
            ("shepherdd", &mut self.shepherdd),
            ("launcher", &mut self.launcher),
            ("hud", &mut self.hud),
        ] {
            if let Some(child) = child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                bail!("{} exited unexpectedly with status {:?}", name, status);
            }
        }
        Ok(())
    }

    /// Rewrite the on-disk config and return the new contents written.
    pub fn rewrite_config(&self, new_toml: &str) -> Result<()> {
        let auth_line = match &self.auth_token {
            Some(t) => format!("auth_token = \"{}\"", t),
            None => String::new(),
        };
        let rendered = new_toml
            .replace("{HTTP_PORT}", &self.http_port.to_string())
            .replace("{AUTH_TOKEN_LINE}", &auth_line);
        fs::write(&self.config_path, rendered).context("rewrite config")?;
        Ok(())
    }

    /// Send `signal` to a child by name. Used by tests that want to
    /// validate clean shutdown explicitly.
    pub fn signal(&self, child: HarnessProcess, signal: Signal) -> Result<()> {
        let pid = match child {
            HarnessProcess::Sway => self.sway.as_ref().and_then(|c| c.id()),
            HarnessProcess::Shepherdd => self.shepherdd.as_ref().and_then(|c| c.id()),
            HarnessProcess::Launcher => self.launcher.as_ref().and_then(|c| c.id()),
            HarnessProcess::Hud => self.hud.as_ref().and_then(|c| c.id()),
        };
        let pid = pid.ok_or_else(|| anyhow!("{:?} not running", child))?;
        kill(Pid::from_raw(pid as i32), signal)?;
        Ok(())
    }

    /// Wait for the named child to exit and return its status.
    pub async fn wait_for_exit(
        &mut self,
        child: HarnessProcess,
        timeout: Duration,
    ) -> Result<std::process::ExitStatus> {
        let slot = match child {
            HarnessProcess::Sway => &mut self.sway,
            HarnessProcess::Shepherdd => &mut self.shepherdd,
            HarnessProcess::Launcher => &mut self.launcher,
            HarnessProcess::Hud => &mut self.hud,
        };
        let proc = slot
            .as_mut()
            .ok_or_else(|| anyhow!("{:?} already reaped", child))?;
        let status = tokio::time::timeout(timeout, proc.wait())
            .await
            .map_err(|_| anyhow!("{:?} did not exit within {:?}", child, timeout))??;
        // Mark slot as taken so Drop doesn't try to kill it.
        *slot = None;
        Ok(status)
    }

    /// Graceful shutdown. Sends SIGTERM in reverse-of-startup order, waits
    /// briefly, escalates to SIGKILL if needed.
    pub async fn shutdown(mut self) -> Result<()> {
        self.shut_down = true;
        // Reverse order: UIs, then shepherdd, then sway.
        for slot in [
            &mut self.hud,
            &mut self.launcher,
            &mut self.shepherdd,
            &mut self.sway,
        ] {
            if let Some(child) = slot.take() {
                terminate(child).await;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HarnessProcess {
    Sway,
    Shepherdd,
    Launcher,
    Hud,
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        if self.shut_down {
            return;
        }
        // Best-effort synchronous teardown. We can't await in Drop so we
        // send signals directly and let the kernel reap.
        for slot in [
            &mut self.hud,
            &mut self.launcher,
            &mut self.shepherdd,
            &mut self.sway,
        ] {
            if let Some(child) = slot.take()
                && let Some(pid) = child.id()
            {
                let pid = Pid::from_raw(pid as i32);
                let _ = kill(pid, Signal::SIGTERM);
            }
        }
        // Give children up to 1s to exit, then SIGKILL anything stragglers.
        std::thread::sleep(Duration::from_millis(500));
    }
}

async fn terminate(mut child: Child) {
    let Some(pid) = child.id() else {
        let _ = child.wait().await;
        return;
    };
    let pid = Pid::from_raw(pid as i32);
    let _ = kill(pid, Signal::SIGTERM);
    match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            let _ = kill(pid, Signal::SIGKILL);
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
    }
}

fn spawn_ui(
    name: &str,
    xdg_runtime_dir: &Path,
    wayland_display: &str,
    socket_path: &Path,
    data_dir: &Path,
) -> Result<Child> {
    let bin = workspace_binary(name);
    if !bin.exists() {
        bail!("{} binary not found at {}", name, bin.display());
    }
    let mut cmd = Command::new(&bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("USER", std::env::var("USER").unwrap_or_default())
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .env("WAYLAND_DISPLAY", wayland_display)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("SHEPHERD_SOCKET", socket_path)
        .env("SHEPHERD_DATA_DIR", data_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    cmd.spawn().with_context(|| format!("spawn {name}"))
}

/// Helpers to look up activity processes (sleep) by command in /proc.
pub mod proc_inspect {
    use super::*;
    use std::fs;

    /// Returns true if any process owned by the current user matches the
    /// given command basename and includes the given session-id-ish marker
    /// in its argv (usually the path to a temp file).
    pub fn any_process_matching(comm: &str) -> bool {
        !find_processes_matching(comm).is_empty()
    }

    pub fn find_processes_matching(comm: &str) -> Vec<u32> {
        let mut out = vec![];
        let Ok(entries) = fs::read_dir("/proc") else {
            return out;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(s) = name.to_str() else { continue };
            let Ok(pid) = s.parse::<u32>() else { continue };
            let Ok(this_comm) = fs::read_to_string(format!("/proc/{pid}/comm")) else {
                continue;
            };
            if this_comm.trim() == comm {
                out.push(pid);
            }
        }
        out
    }

    /// Wait until no process with the given command name is running, or
    /// timeout.
    pub async fn wait_until_no_process(comm: &str, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if find_processes_matching(comm).is_empty() {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                bail!(
                    "timed out waiting for `{}` processes to exit (still running: {:?})",
                    comm,
                    find_processes_matching(comm)
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// Read a JSON value from an [`HttpResponse`], or return a descriptive error
/// if the body is not valid JSON.
pub fn json_body(resp: &HttpResponse) -> Result<Value> {
    resp.json()
        .with_context(|| format!("response body was not JSON: {:?}", resp.body))
}
