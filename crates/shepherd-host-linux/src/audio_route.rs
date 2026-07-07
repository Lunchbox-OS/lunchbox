//! Route audio to an external HDMI/DisplayPort sink while docked (issue #87).
//!
//! When a secondary display is connected the user almost always wants sound on
//! the TV it plugged into, in both mirror and external-only modes. This module
//! switches the PipeWire default sink to the external audio device on dock and
//! restores the previous default on undock.
//!
//! Correlating a DRM connector to its audio sink is inherently heuristic —
//! there is no stable cross-subsystem identifier — so selection is best-effort:
//! we pick the first available `Audio/Sink` whose PipeWire node name or ALSA
//! path marks it HDMI/DisplayPort, and if none is found we leave the default
//! untouched and log. The router is a no-op on non-PipeWire hosts.

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// A PipeWire `Audio/Sink` node discovered from `pw-dump`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkNode {
    pub id: u32,
    pub name: String,
    /// True when the node looks like an HDMI/DisplayPort output.
    pub is_external_video: bool,
}

#[derive(Debug, Deserialize)]
struct RawPwObject {
    #[serde(default)]
    id: Option<u32>,
    #[serde(rename = "type", default)]
    obj_type: String,
    #[serde(default)]
    info: Option<RawPwInfo>,
    #[serde(default)]
    metadata: Vec<RawMetadataEntry>,
}

#[derive(Debug, Deserialize)]
struct RawPwInfo {
    #[serde(default)]
    props: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawMetadataEntry {
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: serde_json::Value,
}

/// True if any of the sink's identifying strings marks it as HDMI/DisplayPort.
fn looks_external_video(name: &str, alsa_path: &str) -> bool {
    let hay = format!("{name} {alsa_path}").to_ascii_lowercase();
    hay.contains("hdmi") || hay.contains("displayport") || hay.contains("display-port")
}

/// Parse `pw-dump` JSON into the list of audio sinks plus the node name that is
/// currently the default sink (from PipeWire's `default.audio.sink` metadata).
fn parse_pw_dump(raw: &[u8]) -> (Vec<SinkNode>, Option<String>) {
    let objs: Vec<RawPwObject> = match serde_json::from_slice(raw) {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "Failed to parse pw-dump output");
            return (Vec::new(), None);
        }
    };

    let mut sinks = Vec::new();
    let mut default_name = None;

    for obj in &objs {
        if obj.obj_type.ends_with("Metadata") {
            for m in &obj.metadata {
                if m.key == "default.audio.sink" {
                    // value is an object like {"name": "alsa_output..."}.
                    if let Some(name) = m.value.get("name").and_then(|v| v.as_str()) {
                        default_name = Some(name.to_string());
                    }
                }
            }
        }
    }

    for obj in objs {
        let (Some(id), Some(info)) = (obj.id, obj.info.as_ref()) else {
            continue;
        };
        let props = &info.props;
        let is_sink = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .map(|c| c == "Audio/Sink")
            .unwrap_or(false);
        if !is_sink {
            continue;
        }
        let name = props
            .get("node.name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let alsa_path = props
            .get("api.alsa.path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let description = props
            .get("node.description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        sinks.push(SinkNode {
            id,
            is_external_video: looks_external_video(&format!("{name} {description}"), alsa_path),
            name,
        });
    }

    (sinks, default_name)
}

/// Pick the external-video sink to route audio to: the first available sink
/// flagged as HDMI/DisplayPort.
fn select_external_sink(sinks: &[SinkNode]) -> Option<&SinkNode> {
    sinks.iter().find(|s| s.is_external_video)
}

/// Switches the default audio sink to an external display's output while docked.
#[async_trait]
pub trait AudioRouter: Send + Sync {
    /// Route audio to an external (HDMI/DisplayPort) sink, remembering the
    /// current default so it can be restored. No-op (logged) when no external
    /// sink is found.
    async fn route_to_external(&self);
    /// Restore the default sink captured by the last [`route_to_external`].
    async fn restore(&self);
}

/// No-op [`AudioRouter`] for tests and hosts where routing is disabled.
pub struct NoOpAudioRouter;

#[async_trait]
impl AudioRouter for NoOpAudioRouter {
    async fn route_to_external(&self) {}
    async fn restore(&self) {}
}

/// PipeWire-backed [`AudioRouter`] using `pw-dump` + `wpctl set-default`.
pub struct PipeWireAudioRouter {
    /// Node id of the pre-dock default sink, saved so undock can restore it.
    saved_default: Mutex<Option<u32>>,
}

impl PipeWireAudioRouter {
    pub fn new() -> Self {
        Self {
            saved_default: Mutex::new(None),
        }
    }

    async fn pw_dump() -> Option<Vec<u8>> {
        let out = tokio::process::Command::new("pw-dump")
            .output()
            .await
            .ok()?;
        out.status.success().then_some(out.stdout)
    }

    async fn set_default(id: u32) -> bool {
        tokio::process::Command::new("wpctl")
            .args(["set-default", &id.to_string()])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

impl Default for PipeWireAudioRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioRouter for PipeWireAudioRouter {
    async fn route_to_external(&self) {
        let Some(dump) = Self::pw_dump().await else {
            warn!("pw-dump unavailable; skipping audio routing");
            return;
        };
        let (sinks, default_name) = parse_pw_dump(&dump);
        let Some(external) = select_external_sink(&sinks) else {
            warn!("No HDMI/DisplayPort audio sink found; leaving default unchanged");
            return;
        };
        let current_default_id = default_name
            .as_deref()
            .and_then(|n| sinks.iter().find(|s| s.name == n))
            .map(|s| s.id);
        if current_default_id == Some(external.id) {
            return; // already the default
        }
        let mut saved = self.saved_default.lock().await;
        // Only remember the original default the first time we divert, so a
        // dock→toggle→undock sequence still restores the true pre-dock sink.
        if saved.is_none() {
            *saved = current_default_id;
        }
        if Self::set_default(external.id).await {
            info!(sink = %external.name, "Routed audio to external display");
        } else {
            warn!(sink = %external.name, "Failed to set external audio sink as default");
        }
    }

    async fn restore(&self) {
        let Some(id) = self.saved_default.lock().await.take() else {
            return;
        };
        if Self::set_default(id).await {
            info!(sink_id = id, "Restored default audio sink");
        } else {
            warn!(sink_id = id, "Failed to restore default audio sink");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sinks_and_default_and_flags_hdmi() {
        let json = br#"[
          {"id":40,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Audio/Sink",
            "node.name":"alsa_output.pci-0000_00_1f.3.analog-stereo",
            "node.description":"Built-in Audio"}}},
          {"id":51,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Audio/Sink",
            "node.name":"alsa_output.pci-0000_01_00.1.hdmi-stereo",
            "api.alsa.path":"hdmi:0",
            "node.description":"GPU HDMI Audio"}}},
          {"id":99,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Stream/Output/Audio","node.name":"some-app"}}},
          {"id":7,"type":"PipeWire:Interface:Metadata","metadata":[
            {"key":"default.audio.sink","value":{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}}
          ]}
        ]"#;
        let (sinks, default_name) = parse_pw_dump(json);
        // Only the two Audio/Sink nodes, not the stream.
        assert_eq!(sinks.len(), 2);
        assert_eq!(
            default_name.as_deref(),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
        let external = select_external_sink(&sinks).unwrap();
        assert_eq!(external.id, 51);
        assert!(external.is_external_video);
        // The analog built-in must not be mistaken for external video.
        assert!(!sinks.iter().find(|s| s.id == 40).unwrap().is_external_video);
    }

    #[test]
    fn no_external_sink_returns_none() {
        let json = br#"[
          {"id":40,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Audio/Sink","node.name":"alsa_output.analog-stereo"}}}
        ]"#;
        let (sinks, _) = parse_pw_dump(json);
        assert!(select_external_sink(&sinks).is_none());
    }
}
