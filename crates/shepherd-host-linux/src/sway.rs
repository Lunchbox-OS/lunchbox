//! Sway compositor helpers
//!
//! Today this is a thin wrapper around `swaymsg -t get_tree` that flattens
//! the tree into a list of [`WindowInfo`] for the debug endpoint. We shell
//! out to `swaymsg` rather than speak the IPC socket directly because the
//! rest of the adapter already does this (`swaymsg exit` in `logout`) and
//! the tree query happens infrequently and off the hot path.
//!
//! The scratchpad in sway is a hidden pseudo-workspace named `__i3_scratch`.
//! Windows moved there with `move scratchpad` (see `sway.conf` for the
//! Steam client) live under that workspace's `floating_nodes`.

use serde::Deserialize;
use shepherd_api::{WindowAction, WindowInfo};
use shepherd_host_api::{HostError, HostResult};

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
}

/// Sway returns a JSON array of `{success, error?}` objects for run_command
/// requests. Exit status is 0 even when the command failed against the tree,
/// so we have to inspect the response.
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

/// Perform a debug action on the window with the given sway con_id.
pub async fn act_on_window(window_id: u64, action: WindowAction) -> HostResult<()> {
    let verb = match action {
        WindowAction::Close => "kill",
        WindowAction::Hide => "move scratchpad",
        WindowAction::Show => "scratchpad show",
    };
    let cmd = format!("[con_id={window_id}] {verb}");
    let output = tokio::process::Command::new("swaymsg")
        .arg(&cmd)
        .output()
        .await
        .map_err(|e| HostError::Internal(format!("failed to invoke swaymsg: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HostError::Internal(format!(
            "swaymsg exited non-zero: {stderr}"
        )));
    }
    let replies: Vec<CommandReply> = serde_json::from_slice(&output.stdout)
        .map_err(|e| HostError::Internal(format!("failed to parse swaymsg reply: {e}")))?;
    for reply in replies {
        if !reply.success {
            let err = reply.error.unwrap_or_else(|| "unknown sway error".into());
            return Err(HostError::Internal(format!(
                "swaymsg `{cmd}` failed: {err}"
            )));
        }
    }
    Ok(())
}

/// Run `swaymsg -t get_outputs` and return one [`OutputScale`] per active
/// output. Inactive outputs (disconnected, off) are skipped because we have
/// nothing to restore for them.
pub async fn get_outputs() -> HostResult<Vec<OutputScale>> {
    let output = tokio::process::Command::new("swaymsg")
        .args(["-t", "get_outputs", "--raw"])
        .output()
        .await
        .map_err(|e| HostError::Internal(format!("failed to invoke swaymsg: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HostError::Internal(format!(
            "swaymsg get_outputs failed: {stderr}"
        )));
    }
    parse_outputs(&output.stdout)
}

fn parse_outputs(raw: &[u8]) -> HostResult<Vec<OutputScale>> {
    let raws: Vec<RawOutput> = serde_json::from_slice(raw)
        .map_err(|e| HostError::Internal(format!("failed to parse swaymsg outputs: {e}")))?;
    Ok(raws
        .into_iter()
        .filter(|o| o.active)
        .map(|o| OutputScale {
            name: o.name,
            scale: o.scale.unwrap_or(1.0),
        })
        .collect())
}

/// Set the scale on a named output via `swaymsg output <name> scale <s>`.
/// Sway accepts fractional scales like 1.25; passing 1.0 disables scaling.
pub async fn set_output_scale(name: &str, scale: f64) -> HostResult<()> {
    let cmd = format!("output {name} scale {scale}");
    let output = tokio::process::Command::new("swaymsg")
        .arg(&cmd)
        .output()
        .await
        .map_err(|e| HostError::Internal(format!("failed to invoke swaymsg: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HostError::Internal(format!(
            "swaymsg exited non-zero: {stderr}"
        )));
    }
    let replies: Vec<CommandReply> = serde_json::from_slice(&output.stdout)
        .map_err(|e| HostError::Internal(format!("failed to parse swaymsg reply: {e}")))?;
    for reply in replies {
        if !reply.success {
            let err = reply.error.unwrap_or_else(|| "unknown sway error".into());
            return Err(HostError::Internal(format!(
                "swaymsg `{cmd}` failed: {err}"
            )));
        }
    }
    Ok(())
}

/// Return true if `window` looks like a Steam dialog that is currently
/// stranded in the scratchpad — i.e. it has the Steam X11 class but its
/// title doesn't match the patterns we deliberately hide via `sway.conf`
/// (`Steam` for the main client, `Steam - ...` for its sub-windows).
///
/// These are the dialogs that block `steam://rungameid/...` launches when
/// Steam can't reach its servers ("You appear to be offline", "Connection
/// Error", etc.). Anything matching this is a candidate to surface from
/// the launch watchdog in the host adapter; see issue #50.
fn is_stuck_steam_dialog(window: &WindowInfo) -> bool {
    if !window.in_scratchpad {
        return false;
    }
    let class_matches = window
        .window_class
        .as_deref()
        .is_some_and(|c| c.eq_ignore_ascii_case("steam"));
    if !class_matches {
        return false;
    }
    let title_is_known_client = window
        .name
        .as_deref()
        .is_some_and(|t| t == "Steam" || t.starts_with("Steam - "));
    !title_is_known_client
}

/// Find any Steam dialog stuck in the scratchpad and pull each one out so
/// the user can interact with it. Returns the number of windows surfaced.
///
/// Used by [`crate::adapter::LinuxHost`]'s launch watchdog: when a Steam
/// `steam://rungameid/...` launch hasn't produced a game window in time,
/// chances are the launch is blocked on a hidden modal (see issue #50).
pub async fn surface_stuck_steam_dialogs() -> HostResult<Vec<u64>> {
    let windows = list_windows().await?;
    let stuck: Vec<u64> = windows
        .iter()
        .filter(|w| is_stuck_steam_dialog(w))
        .map(|w| w.id)
        .collect();
    for id in &stuck {
        // Best-effort: log and continue if a single window fails so we
        // still try to surface the others.
        if let Err(e) = act_on_window(*id, WindowAction::Show).await {
            tracing::warn!(window_id = id, error = %e, "Failed to surface stuck Steam dialog");
        }
    }
    Ok(stuck)
}

/// Run `swaymsg -t get_tree` and return a flattened window list.
pub async fn list_windows() -> HostResult<Vec<WindowInfo>> {
    let output = tokio::process::Command::new("swaymsg")
        .args(["-t", "get_tree", "--raw"])
        .output()
        .await
        .map_err(|e| HostError::Internal(format!("failed to invoke swaymsg: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HostError::Internal(format!(
            "swaymsg get_tree failed: {stderr}"
        )));
    }
    let root: Node = serde_json::from_slice(&output.stdout)
        .map_err(|e| HostError::Internal(format!("failed to parse swaymsg output: {e}")))?;
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

    fn make_window(
        id: u64,
        name: Option<&str>,
        class: Option<&str>,
        in_scratchpad: bool,
    ) -> WindowInfo {
        WindowInfo {
            id,
            name: name.map(str::to_string),
            app_id: None,
            window_class: class.map(str::to_string),
            pid: Some(1234),
            workspace: if in_scratchpad {
                Some(SCRATCHPAD_WORKSPACE.into())
            } else {
                Some("1".into())
            },
            in_scratchpad,
            visible: !in_scratchpad,
            focused: false,
        }
    }

    #[test]
    fn surfaces_unrecognized_steam_dialogs_only() {
        // Main client and known sub-windows must stay hidden — they are
        // intentionally moved to the scratchpad by sway.conf.
        let main_client = make_window(1, Some("Steam"), Some("Steam"), true);
        let news = make_window(2, Some("Steam - News"), Some("Steam"), true);
        // Offline/connection-error dialogs are stuck in the scratchpad
        // when sway's broad class-based rules catch them (pre-#50) or if
        // Steam reuses the class for a new modal we don't know about.
        let offline_dialog = make_window(3, Some("Connection Error"), Some("Steam"), true);
        let sign_in = make_window(4, Some("Sign In"), Some("Steam"), true);
        // A dialog Steam mapped without a title yet — surface it too, the
        // user still needs to be able to dismiss it.
        let untitled = make_window(5, None, Some("Steam"), true);
        // Non-Steam scratchpadded windows must not be touched.
        let other_app = make_window(6, Some("Whatever"), Some("Firefox"), true);
        // A Steam dialog already on a real workspace doesn't need
        // surfacing — it's already visible.
        let visible_dialog = make_window(7, Some("Connection Error"), Some("Steam"), false);

        assert!(!is_stuck_steam_dialog(&main_client));
        assert!(!is_stuck_steam_dialog(&news));
        assert!(is_stuck_steam_dialog(&offline_dialog));
        assert!(is_stuck_steam_dialog(&sign_in));
        assert!(is_stuck_steam_dialog(&untitled));
        assert!(!is_stuck_steam_dialog(&other_app));
        assert!(!is_stuck_steam_dialog(&visible_dialog));
    }

    #[test]
    fn steam_class_match_is_case_insensitive() {
        // Older Steam X11 builds report `steam` (lowercase) in WM_CLASS.
        let dialog = make_window(1, Some("Connection Error"), Some("steam"), true);
        assert!(is_stuck_steam_dialog(&dialog));
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
