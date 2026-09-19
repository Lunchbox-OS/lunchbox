//! Sway compositor helpers
//!
//! Queries and commands over the compositor's IPC socket, via
//! [`crate::sway_ipc`]. This used to spawn a `swaymsg` subprocess per call,
//! which stopped being defensible once `list_windows` moved onto the
//! supervision path: the escape and orphan sweeps read the tree every two
//! seconds for the whole uptime of the daemon (issue #147).
//!
//! The parsing below (`parse_outputs`, `parse_displays`, `walk`) is
//! deliberately separate from the transport and takes bytes, so it stays
//! testable against literals without a compositor.
//!
//! The scratchpad in sway is a hidden pseudo-workspace named `__i3_scratch`.
//! Windows moved there with `move scratchpad` (see `sway.conf` for the
//! Steam client) live under that workspace's `floating_nodes`.

use lunchbox_api::{VideoMode, WindowAction, WindowInfo, WindowOwner};
use lunchbox_host_api::{HostError, HostResult};
use serde::Deserialize;

const SCRATCHPAD_WORKSPACE: &str = "__i3_scratch";

/// Per-output scale snapshot returned by [`get_outputs`]. Used to capture
/// the pre-launch scale so we can restore it after an XWayland activity
/// exits (see issue #45).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputScale {
    pub name: String,
    pub scale: f64,
}

#[derive(Debug, Deserialize)]
struct RawOutput {
    name: String,
    #[serde(default)]
    scale: Option<f64>,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    make: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    current_mode: Option<RawMode>,
    #[serde(default)]
    modes: Vec<RawMode>,
}

/// A `{width, height, refresh}` entry from sway's `get_outputs`. `refresh` is
/// millihertz.
#[derive(Debug, Clone, Copy, Deserialize)]
struct RawMode {
    width: u32,
    height: u32,
    #[serde(default)]
    refresh: u32,
}

impl From<RawMode> for VideoMode {
    fn from(m: RawMode) -> Self {
        VideoMode {
            width: m.width,
            height: m.height,
            refresh_mhz: m.refresh,
        }
    }
}

/// A connected compositor output and its capabilities, used to drive the
/// external-display / docking arrangement (issue #87). Richer than
/// [`OutputScale`], which only carries what the HiDPI workaround needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub name: String,
    /// Whether the output is currently enabled and driving a signal.
    pub active: bool,
    /// Whether this output holds keyboard focus.
    pub focused: bool,
    pub make: Option<String>,
    pub model: Option<String>,
    /// The mode the output is currently running, if active.
    pub current_mode: Option<VideoMode>,
    /// Every mode the output advertises (may be empty for a disabled output).
    pub modes: Vec<VideoMode>,
}

impl DisplayInfo {
    /// Heuristic for an internal laptop/handheld panel by connector name.
    /// Not used for primary selection (issue #87 fixes primary to
    /// first-enumerated) but handy for logging and future policy.
    pub fn is_internal(&self) -> bool {
        let n = self.name.to_ascii_lowercase();
        n.starts_with("edp") || n.starts_with("lvds") || n.starts_with("dsi")
    }
}

/// Sway returns a JSON array of `{success, error?}` objects for `RUN_COMMAND`
/// requests. A command that failed against the tree is reported here and
/// nowhere else, so every reply is inspected — see [`run_command`].
#[derive(Debug, Deserialize)]
struct CommandReply {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: u64,
    #[serde(rename = "type", default)]
    node_type: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    visible: Option<bool>,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    window_properties: Option<WindowProperties>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    floating_nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct WindowProperties {
    #[serde(default)]
    class: Option<String>,
}

/// Run one sway command and check its reply.
///
/// A `RUN_COMMAND` reply is an array of `{success, error?}` — sway reports a
/// command that failed against the tree in the payload, not in any status. This
/// is the single place that checks it, which is what the `swaymsg` era could
/// not have: there, every caller had to remember to re-inspect stdout.
async fn run_command(cmd: &str) -> HostResult<()> {
    let body = crate::sway_ipc::client()
        .request(crate::sway_ipc::RUN_COMMAND, cmd.as_bytes())
        .await?;
    check_command_replies(cmd, &body)
}

/// Inspect a `RUN_COMMAND` reply array and turn any failure into an error.
fn check_command_replies(cmd: &str, body: &[u8]) -> HostResult<()> {
    let replies: Vec<CommandReply> = serde_json::from_slice(body)
        .map_err(|e| HostError::Internal(format!("failed to parse sway's command reply: {e}")))?;
    for reply in replies {
        if !reply.success {
            let err = reply.error.unwrap_or_else(|| "unknown sway error".into());
            return Err(HostError::Internal(format!("sway `{cmd}` failed: {err}")));
        }
    }
    Ok(())
}

/// Ask sway for the current output list, as raw JSON.
async fn get_outputs_raw() -> HostResult<Vec<u8>> {
    crate::sway_ipc::client()
        .request(crate::sway_ipc::GET_OUTPUTS, b"")
        .await
}

/// End the compositor session (`swaymsg exit`, as was).
///
/// A connection that dies without replying counts as success: sway may tear
/// the socket down before it gets round to answering, and either way the
/// session is over. A reply that says the command *failed* is a real error —
/// sway is still up and still refusing.
pub async fn exit() -> HostResult<()> {
    let Some(body) = crate::sway_ipc::client()
        .request_tolerating_disconnect(crate::sway_ipc::RUN_COMMAND, b"exit")
        .await?
    else {
        return Ok(());
    };
    check_command_replies("exit", &body)
}

/// Turn every output on or off (DPMS).
///
/// `swayidle` used to do this itself with `swaymsg "output * dpms off"`, which
/// stopped working the moment the compositor socket lost its name (issue #144)
/// — silently, because nothing checks a `swayidle` command's exit status. The
/// timer still lives in `swayidle`; only the privileged half moved here, onto
/// the connection this daemon holds for the life of the session.
pub async fn set_screen_power(on: bool) -> HostResult<()> {
    run_command(if on {
        "output * dpms on"
    } else {
        "output * dpms off"
    })
    .await
}

/// Switch the compositor's active binding mode.
///
/// The kiosk's key grabs live in `sway.conf`'s default mode and the relaxed set
/// in `mode "admin"` (issue #154); switching between them is how admin mode
/// hands `Home`, `Ctrl+w` and `Alt+F4` back to whatever is on screen. Named
/// modes are the one part of the config that *is* switchable at runtime —
/// `for_window` rules are evaluated at map time and stay as the kiosk set them.
///
/// This is deliberately reachable only from the host adapter: a UI client
/// shelling out to `swaymsg` would work in dev and fail on a device that has
/// hardened sway's IPC socket.
pub async fn set_binding_mode(mode: &str) -> HostResult<()> {
    run_command(&format!("mode \"{mode}\"")).await
}

/// Perform an action on the window with the given sway con_id.
pub async fn act_on_window(window_id: u64, action: WindowAction) -> HostResult<()> {
    let verb = match action {
        WindowAction::Close => "kill",
        WindowAction::Hide => "move scratchpad",
        WindowAction::Show => "scratchpad show",
        WindowAction::Focus => "focus",
    };
    run_command(&format!("[con_id={window_id}] {verb}")).await
}

/// Return one [`OutputScale`] per active output. Inactive outputs
/// (disconnected, off) are skipped because we have nothing to restore for them.
pub async fn get_outputs() -> HostResult<Vec<OutputScale>> {
    parse_outputs(&get_outputs_raw().await?)
}

fn parse_outputs(raw: &[u8]) -> HostResult<Vec<OutputScale>> {
    let raws: Vec<RawOutput> = serde_json::from_slice(raw)
        .map_err(|e| HostError::Internal(format!("failed to parse sway's outputs: {e}")))?;
    Ok(raws
        .into_iter()
        .filter(|o| o.active)
        .map(|o| OutputScale {
            name: o.name,
            scale: o.scale.unwrap_or(1.0),
        })
        .collect())
}

/// Set the scale on a named output via `output <name> scale <s>`.
/// Sway accepts fractional scales like 1.25; passing 1.0 disables scaling.
pub async fn set_output_scale(name: &str, scale: f64) -> HostResult<()> {
    run_command(&format!("output {name} scale {scale}")).await
}

/// Return one [`DisplayInfo`] per output, including disabled/disconnected ones
/// (unlike [`get_outputs`], which filters to active). Order matches sway's
/// enumeration, which is what issue #87's "primary is first-enumerated" rule
/// keys off of.
pub async fn get_displays() -> HostResult<Vec<DisplayInfo>> {
    parse_displays(&get_outputs_raw().await?)
}

fn parse_displays(raw: &[u8]) -> HostResult<Vec<DisplayInfo>> {
    let raws: Vec<RawOutput> = serde_json::from_slice(raw)
        .map_err(|e| HostError::Internal(format!("failed to parse sway's outputs: {e}")))?;
    Ok(raws
        .into_iter()
        .map(|o| DisplayInfo {
            name: o.name,
            active: o.active,
            focused: o.focused,
            make: o.make,
            model: o.model,
            current_mode: o.current_mode.map(VideoMode::from),
            modes: o.modes.into_iter().map(VideoMode::from).collect(),
        })
        .collect())
}

/// Set an output's video mode via `output <name> mode <WxH@RHz>`.
/// A `refresh_mhz` of 0 (unknown) omits the refresh so sway picks a default
/// for the resolution.
pub async fn set_output_mode(name: &str, mode: VideoMode) -> HostResult<()> {
    let spec = if mode.refresh_mhz > 0 {
        format!(
            "{}x{}@{:.3}Hz",
            mode.width,
            mode.height,
            f64::from(mode.refresh_mhz) / 1000.0
        )
    } else {
        format!("{}x{}", mode.width, mode.height)
    };
    run_command(&format!("output {name} mode {spec}")).await
}

/// Confine the seat's relative pointer(s) to a single output via
/// `input type:pointer map_to_output <name>`, or pass `"*"` to release
/// the confinement back to the whole layout. Used in mirror mode to keep the
/// cursor on the interactive primary so it can't wander onto the uninteractive
/// wl-mirror surface (issue #87). `map_to_output` is documented to apply to
/// pointer devices, so it constrains a relative mouse, not just absolute ones.
pub async fn map_pointer_to_output(output: &str) -> HostResult<()> {
    run_command(&format!("input type:pointer map_to_output {output}")).await
}

/// Enable an output via `output <name> enable`.
pub async fn enable_output(name: &str) -> HostResult<()> {
    run_command(&format!("output {name} enable")).await
}

/// Disable an output via `output <name> disable`. Sway migrates any
/// workspace on the output to a remaining active output, so the single kiosk
/// workspace (and its activity) is never lost.
pub async fn disable_output(name: &str) -> HostResult<()> {
    run_command(&format!("output {name} disable")).await
}

/// Move the window matching `criteria` to `output` and fullscreen it. Used to
/// pin the `wl-mirror` client to the external display (issue #87).
pub async fn move_to_output_fullscreen(criteria: &str, output: &str) -> HostResult<()> {
    run_command(&format!(
        "[{criteria}] move container to output {output}, fullscreen enable"
    ))
    .await
}

/// Select the primary output: the first-enumerated one (issue #87). Returns
/// `None` only when no outputs are present.
pub fn select_primary(displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    displays.first()
}

/// Pick the logical mode to drive the primary output at while mirroring onto
/// `secondary` (issue #87): the highest-resolution mode both panels advertise,
/// so the mirror is a clean copy and the hardware upscales where its native
/// resolution is larger. Falls back to the primary's current mode (then its own
/// highest mode) when the panels share no common resolution.
pub fn pick_mirror_mode(primary: &DisplayInfo, secondary: &DisplayInfo) -> Option<VideoMode> {
    // Compare by resolution only — a mode present on both at any refresh is a
    // valid mirror target. Rank by pixel area, then by refresh for a stable
    // pick among equal-area modes.
    let common = primary
        .modes
        .iter()
        .filter(|pm| {
            secondary
                .modes
                .iter()
                .any(|sm| sm.width == pm.width && sm.height == pm.height)
        })
        .copied()
        .max_by(|a, b| {
            a.area()
                .cmp(&b.area())
                .then(a.refresh_mhz.cmp(&b.refresh_mhz))
        });

    common.or(primary.current_mode).or_else(|| {
        primary.modes.iter().copied().max_by(|a, b| {
            a.area()
                .cmp(&b.area())
                .then(a.refresh_mhz.cmp(&b.refresh_mhz))
        })
    })
}

/// Compositor output operations behind a trait so the docking state machine in
/// `lunchboxd` can be unit-tested against a mock without a live sway. The
/// production implementation is [`SwayIpcBackend`].
#[async_trait::async_trait]
pub trait OutputBackend: Send + Sync {
    async fn get_displays(&self) -> HostResult<Vec<DisplayInfo>>;
    async fn set_output_mode(&self, name: &str, mode: VideoMode) -> HostResult<()>;
    async fn set_output_scale(&self, name: &str, scale: f64) -> HostResult<()>;
    async fn enable_output(&self, name: &str) -> HostResult<()>;
    async fn disable_output(&self, name: &str) -> HostResult<()>;
    async fn move_to_output_fullscreen(&self, criteria: &str, output: &str) -> HostResult<()>;
    /// Confine relative pointers to `output`, or release with `"*"`.
    async fn map_pointer_to_output(&self, output: &str) -> HostResult<()>;
}

/// Production [`OutputBackend`], talking to sway over its IPC socket.
pub struct SwayIpcBackend;

#[async_trait::async_trait]
impl OutputBackend for SwayIpcBackend {
    async fn get_displays(&self) -> HostResult<Vec<DisplayInfo>> {
        get_displays().await
    }
    async fn set_output_mode(&self, name: &str, mode: VideoMode) -> HostResult<()> {
        set_output_mode(name, mode).await
    }
    async fn set_output_scale(&self, name: &str, scale: f64) -> HostResult<()> {
        set_output_scale(name, scale).await
    }
    async fn enable_output(&self, name: &str) -> HostResult<()> {
        enable_output(name).await
    }
    async fn disable_output(&self, name: &str) -> HostResult<()> {
        disable_output(name).await
    }
    async fn move_to_output_fullscreen(&self, criteria: &str, output: &str) -> HostResult<()> {
        move_to_output_fullscreen(criteria, output).await
    }
    async fn map_pointer_to_output(&self, output: &str) -> HostResult<()> {
        map_pointer_to_output(output).await
    }
}

/// Read sway's node tree and return a flattened window list.
///
/// `Err` means the compositor could not be asked, which is emphatically not the
/// same as an empty screen — see [`crate::LinuxHost::start_monitor`], where the
/// difference decides whether an escaped activity gets its window closed.
pub async fn list_windows() -> HostResult<Vec<WindowInfo>> {
    let body = crate::sway_ipc::client()
        .request(crate::sway_ipc::GET_TREE, b"")
        .await?;
    let root: Node = serde_json::from_slice(&body)
        .map_err(|e| HostError::Internal(format!("failed to parse sway's tree: {e}")))?;
    let mut out = Vec::new();
    walk(&root, None, &mut out);
    Ok(out)
}

fn walk(node: &Node, workspace: Option<&str>, out: &mut Vec<WindowInfo>) {
    let child_workspace = if node.node_type == "workspace" {
        node.name.as_deref()
    } else {
        workspace
    };

    if is_window(node) {
        let ws = child_workspace.map(str::to_string);
        out.push(WindowInfo {
            id: node.id,
            name: node.name.clone(),
            app_id: node.app_id.clone(),
            window_class: node
                .window_properties
                .as_ref()
                .and_then(|p| p.class.clone()),
            pid: node.pid,
            in_scratchpad: ws.as_deref() == Some(SCRATCHPAD_WORKSPACE),
            workspace: ws,
            visible: node.visible.unwrap_or(false),
            focused: node.focused,
            // The compositor knows pids, not who is supervising them, so
            // every window parses as unowned. `LinuxHost::list_windows`
            // attributes them before they reach a client; the internal
            // callers here match on pids and ignore this field.
            owner: WindowOwner::Unowned,
        });
    }

    for child in &node.nodes {
        walk(child, child_workspace, out);
    }
    for child in &node.floating_nodes {
        walk(child, child_workspace, out);
    }
}

/// A leaf node represents a window if it has no children and reports either
/// an app_id (Wayland), an X11 class, or a pid. Sway uses `con` for tiled
/// windows and `floating_con` for floating ones; both can be windows.
fn is_window(node: &Node) -> bool {
    if !node.nodes.is_empty() || !node.floating_nodes.is_empty() {
        return false;
    }
    matches!(node.node_type.as_str(), "con" | "floating_con")
        && (node.app_id.is_some()
            || node.pid.is_some()
            || node
                .window_properties
                .as_ref()
                .and_then(|p| p.class.as_deref())
                .is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Vec<WindowInfo> {
        let root: Node = serde_json::from_str(json).unwrap();
        let mut out = Vec::new();
        walk(&root, None, &mut out);
        out
    }

    #[test]
    fn flattens_workspace_and_scratchpad() {
        let tree = r#"{
          "id": 1, "type": "root", "nodes": [
            {"id": 2, "type": "output", "name": "HDMI-A-1", "nodes": [
              {"id": 3, "type": "workspace", "name": "1", "nodes": [
                {"id": 10, "type": "con", "name": "Firefox", "app_id": "firefox",
                 "pid": 1234, "visible": true, "focused": true}
              ], "floating_nodes": []},
              {"id": 4, "type": "workspace", "name": "__i3_scratch",
               "nodes": [], "floating_nodes": [
                {"id": 20, "type": "floating_con", "name": "Steam",
                 "window_properties": {"class": "Steam"}, "pid": 5678,
                 "visible": false, "focused": false}
              ]}
            ], "floating_nodes": []}
          ], "floating_nodes": []
        }"#;
        let windows = parse(tree);
        assert_eq!(windows.len(), 2);

        let firefox = windows.iter().find(|w| w.id == 10).unwrap();
        assert_eq!(firefox.app_id.as_deref(), Some("firefox"));
        assert_eq!(firefox.workspace.as_deref(), Some("1"));
        assert!(!firefox.in_scratchpad);
        assert!(firefox.visible);
        assert!(firefox.focused);

        let steam = windows.iter().find(|w| w.id == 20).unwrap();
        assert_eq!(steam.window_class.as_deref(), Some("Steam"));
        assert_eq!(steam.workspace.as_deref(), Some("__i3_scratch"));
        assert!(steam.in_scratchpad);
        assert!(!steam.visible);
    }

    #[test]
    fn parses_active_outputs_with_scale() {
        // The `--raw` flag yields a JSON array of outputs; inactive entries
        // (disconnected, or `output * disable`d) lack a meaningful scale and
        // are filtered out so we don't try to restore them later.
        let json = br#"[
          {"name": "HDMI-A-1", "active": true, "scale": 1.5},
          {"name": "eDP-1", "active": false, "scale": 1.0},
          {"name": "HDMI-A-2", "active": true}
        ]"#;
        let outputs = parse_outputs(json).unwrap();
        assert_eq!(
            outputs,
            vec![
                OutputScale {
                    name: "HDMI-A-1".into(),
                    scale: 1.5,
                },
                OutputScale {
                    name: "HDMI-A-2".into(),
                    scale: 1.0,
                },
            ]
        );
    }

    fn disp(name: &str, modes: &[(u32, u32, u32)]) -> DisplayInfo {
        DisplayInfo {
            name: name.into(),
            active: true,
            focused: false,
            make: None,
            model: None,
            current_mode: modes.first().map(|&(w, h, r)| VideoMode {
                width: w,
                height: h,
                refresh_mhz: r,
            }),
            modes: modes
                .iter()
                .map(|&(w, h, r)| VideoMode {
                    width: w,
                    height: h,
                    refresh_mhz: r,
                })
                .collect(),
        }
    }

    #[test]
    fn parses_displays_including_inactive() {
        let json = br#"[
          {"name":"eDP-1","active":true,"focused":true,"make":"BOE","model":"X",
           "current_mode":{"width":1280,"height":800,"refresh":60000},
           "modes":[{"width":1280,"height":800,"refresh":60000}]},
          {"name":"HDMI-A-1","active":false,"modes":[]}
        ]"#;
        let d = parse_displays(json).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].name, "eDP-1");
        assert!(d[0].active && d[0].focused);
        assert!(d[0].is_internal());
        assert_eq!(
            d[0].current_mode,
            Some(VideoMode {
                width: 1280,
                height: 800,
                refresh_mhz: 60000
            })
        );
        // Inactive/disconnected outputs are retained (needed to know a
        // connector exists before enabling it).
        assert!(!d[1].active);
        assert!(!d[1].is_internal());
    }

    #[test]
    fn select_primary_is_first_enumerated() {
        let displays = vec![disp("HDMI-A-1", &[(1920, 1080, 60000)]), disp("eDP-1", &[])];
        assert_eq!(select_primary(&displays).unwrap().name, "HDMI-A-1");
        assert!(select_primary(&[]).is_none());
    }

    #[test]
    fn pick_mirror_mode_prefers_highest_common_resolution() {
        let primary = disp(
            "eDP-1",
            &[(1920, 1080, 60000), (1280, 800, 60000), (1280, 720, 60000)],
        );
        let secondary = disp("HDMI-A-1", &[(3840, 2160, 60000), (1280, 720, 60000)]);
        // Only 1280x720 is common to both.
        assert_eq!(
            pick_mirror_mode(&primary, &secondary),
            Some(VideoMode {
                width: 1280,
                height: 720,
                refresh_mhz: 60000
            })
        );
    }

    #[test]
    fn pick_mirror_mode_falls_back_to_primary_current_when_no_common_mode() {
        // 16:10 handheld vs 16:9 TV share no resolution.
        let primary = disp("eDP-1", &[(1280, 800, 60000)]);
        let secondary = disp("HDMI-A-1", &[(1920, 1080, 60000)]);
        assert_eq!(
            pick_mirror_mode(&primary, &secondary),
            Some(VideoMode {
                width: 1280,
                height: 800,
                refresh_mhz: 60000
            })
        );
    }

    #[test]
    fn skips_containers_with_children() {
        // Splits/tabbed containers have children — they are not windows.
        let tree = r#"{
          "id": 1, "type": "root", "nodes": [
            {"id": 2, "type": "workspace", "name": "2", "nodes": [
              {"id": 3, "type": "con", "name": "split", "nodes": [
                {"id": 11, "type": "con", "name": "term", "app_id": "alacritty",
                 "pid": 99}
              ], "floating_nodes": []}
            ], "floating_nodes": []}
          ], "floating_nodes": []
        }"#;
        let windows = parse(tree);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, 11);
    }
}
