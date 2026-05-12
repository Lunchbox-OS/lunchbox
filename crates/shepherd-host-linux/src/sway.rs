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
use shepherd_api::WindowInfo;
use shepherd_host_api::{HostError, HostResult};

const SCRATCHPAD_WORKSPACE: &str = "__i3_scratch";

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
