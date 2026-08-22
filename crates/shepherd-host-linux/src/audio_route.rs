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
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::audio::{self, SinkNode};

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
}

impl Default for PipeWireAudioRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioRouter for PipeWireAudioRouter {
    async fn route_to_external(&self) {
        let Some(topo) = audio::dump().await else {
            warn!("pw-dump unavailable; skipping audio routing");
            return;
        };
        let (sinks, default_name) = (topo.sinks, topo.default_sink);
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
        if audio::set_default_sink(external.id).await {
            info!(sink = %external.name, "Routed audio to external display");
        } else {
            warn!(sink = %external.name, "Failed to set external audio sink as default");
        }
    }

    async fn restore(&self) {
        let Some(id) = self.saved_default.lock().await.take() else {
            return;
        };
        if audio::set_default_sink(id).await {
            info!(sink_id = id, "Restored default audio sink");
        } else {
            warn!(sink_id = id, "Failed to restore default audio sink");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::parse_pw_dump;

    #[test]
    fn picks_the_hdmi_sink_as_the_external_target() {
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
          {"id":7,"type":"PipeWire:Interface:Metadata","metadata":[
            {"key":"default.audio.sink","value":{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}}
          ]}
        ]"#;
        let topo = parse_pw_dump(json);
        assert_eq!(
            topo.default_sink.as_deref(),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo")
        );
        assert_eq!(select_external_sink(&topo.sinks).unwrap().id, 51);
    }

    #[test]
    fn no_external_sink_returns_none() {
        let json = br#"[
          {"id":40,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Audio/Sink","node.name":"alsa_output.analog-stereo"}}}
        ]"#;
        let topo = parse_pw_dump(json);
        assert!(select_external_sink(&topo.sinks).is_none());
    }
}
