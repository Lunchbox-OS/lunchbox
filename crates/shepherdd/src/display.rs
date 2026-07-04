//! External monitor / docking support (issue #87).
//!
//! [`DisplayManager`] owns the compositor's display arrangement. On boot it
//! records the primary (first-enumerated) output; when an external display is
//! connected it mirrors the primary onto it by default, and it exposes a toggle
//! to disable the primary and drive the external at its native resolution.
//! Exactly one logical output is ever active, so the one-activity-at-a-time
//! invariant always holds.
//!
//! Mirroring is done with `wl-mirror` (sway/wlroots cannot mirror from config):
//! a screencopy client that paints the primary output onto a window we pin
//! fullscreen to the external display. Audio is routed to the external device
//! while docked. Every change is broadcast as a `DisplayModeChanged` event so
//! the HUD can show/hide its toggle and re-anchor to the active output.
//!
//! The state machine and mode selection are unit-tested against a mock
//! [`OutputBackend`]; `wl-mirror` and audio are behind traits so tests don't
//! spawn real processes.

use async_trait::async_trait;
use shepherd_api::{DisplayMode, DisplayState, Event, EventPayload, VideoMode};
use shepherd_host_api::DisplayController;
use shepherd_host_linux::{
    AudioRouter, DisplayInfo, OutputBackend, pick_mirror_mode, select_primary,
};
use shepherd_ipc::IpcServer;
use std::process::Stdio;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::{info, warn};

/// The Wayland `app_id` wl-mirror gives its window. Used to pin the mirror
/// surface to the external output and to keep sway's kiosk rules from treating
/// it like an activity.
pub const WL_MIRROR_APP_ID: &str = "at.yrlf.wl_mirror";

/// `map_to_output` value that releases pointer confinement back to the whole
/// layout.
const POINTER_ALL_OUTPUTS: &str = "*";

/// Manages the `wl-mirror` child process. Behind a trait so the controller can
/// be tested without spawning anything.
#[async_trait]
pub trait MirrorLauncher: Send + Sync {
    /// (Re)start mirroring `source_output`. Returns true on success; false means
    /// mirroring is unavailable (e.g. `wl-mirror` is not installed), so the
    /// caller should fall back to a mode that does not require it.
    async fn start(&self, source_output: &str) -> bool;
    /// Stop any running mirror. Idempotent.
    async fn stop(&self);
}

/// Production [`MirrorLauncher`] that spawns `wl-mirror`.
pub struct WlMirrorLauncher {
    child: Mutex<Option<tokio::process::Child>>,
}

impl WlMirrorLauncher {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
        }
    }
}

impl Default for WlMirrorLauncher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MirrorLauncher for WlMirrorLauncher {
    async fn start(&self, source_output: &str) -> bool {
        let mut guard = self.child.lock().await;
        if let Some(mut old) = guard.take() {
            let _ = old.kill().await;
        }
        match tokio::process::Command::new("wl-mirror")
            .arg(source_output)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => {
                info!(source = source_output, "Started wl-mirror");
                *guard = Some(child);
                true
            }
            Err(e) => {
                warn!(error = %e, "Failed to start wl-mirror (is it installed?)");
                false
            }
        }
    }

    async fn stop(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
            info!("Stopped wl-mirror");
        }
    }
}

/// Mutable arrangement state.
#[derive(Debug, Clone)]
struct Inner {
    /// Connector name of the primary output, fixed at first enumeration.
    primary: Option<String>,
    /// The primary's mode at startup, captured before any mirror-driven change
    /// so it can be restored when the external display is disconnected.
    primary_native_mode: Option<VideoMode>,
    /// Connector name of the currently connected external output, if any.
    secondary: Option<String>,
    mode: DisplayMode,
}

impl Inner {
    fn to_state(&self) -> DisplayState {
        DisplayState {
            mode: self.mode,
            primary: self.primary.clone(),
            secondary: self.secondary.clone(),
        }
    }
}

/// Sway-backed [`DisplayController`] implementing the issue #87 state machine.
pub struct DisplayManager {
    backend: Arc<dyn OutputBackend>,
    mirror: Arc<dyn MirrorLauncher>,
    audio: Arc<dyn AudioRouter>,
    /// Whether to route audio to the external display while docked.
    mirror_audio: bool,
    ipc: Arc<IpcServer>,
    event_tx: broadcast::Sender<Event>,
    inner: Mutex<Inner>,
    /// Serializes whole reconcile/apply sequences so a burst of HUD toggles (or
    /// a toggle racing a hotplug) can't interleave enable/disable/mode-set and
    /// wl-mirror start/stop and leave outputs — or the HUD's layer surface — in
    /// a broken state.
    apply_lock: Mutex<()>,
}

impl DisplayManager {
    pub fn new(
        backend: Arc<dyn OutputBackend>,
        mirror: Arc<dyn MirrorLauncher>,
        audio: Arc<dyn AudioRouter>,
        mirror_audio: bool,
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            backend,
            mirror,
            audio,
            mirror_audio,
            ipc,
            event_tx,
            inner: Mutex::new(Inner {
                primary: None,
                primary_native_mode: None,
                secondary: None,
                mode: DisplayMode::SingleInternal,
            }),
            apply_lock: Mutex::new(()),
        }
    }

    fn broadcast(&self, state: DisplayState) {
        let event = Event::new(EventPayload::DisplayModeChanged { state });
        self.ipc.broadcast_event(event.clone());
        let _ = self.event_tx.send(event);
    }

    /// Capture the primary output and apply the initial arrangement. Call once
    /// at startup.
    pub async fn initialize(&self) {
        let displays = match self.backend.get_displays().await {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Failed to query displays at startup; docking disabled");
                return;
            }
        };
        let primary_disp = select_primary(&displays);
        let primary = primary_disp.map(|d| d.name.clone());
        let primary_native_mode = primary_disp.and_then(|d| d.current_mode);
        {
            let mut inner = self.inner.lock().await;
            inner.primary = primary.clone();
            inner.primary_native_mode = primary_native_mode;
        }
        if let Some(p) = &primary {
            info!(primary = %p, mode = ?primary_native_mode, "Captured primary display");
        }
        self.reconcile(&displays, true).await;
    }

    /// Re-evaluate the arrangement against the current display topology. Called
    /// by the hotplug watcher. Idempotent: does nothing when the topology and
    /// mode are already consistent.
    pub async fn on_output_changed(&self) {
        let displays = match self.backend.get_displays().await {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Failed to query displays on hotplug");
                return;
            }
        };
        self.reconcile(&displays, false).await;
    }

    /// Core reconciliation. `initial` forces a broadcast even when nothing
    /// changes, so shells learn the starting state.
    async fn reconcile(&self, displays: &[DisplayInfo], initial: bool) {
        let _serial = self.apply_lock.lock().await;
        let (primary_name, prev_secondary, prev_mode) = {
            let inner = self.inner.lock().await;
            (inner.primary.clone(), inner.secondary.clone(), inner.mode)
        };
        let Some(primary_name) = primary_name else {
            return; // never initialized / no outputs
        };

        // The secondary is any connected output other than the primary. Prefer
        // one that reports a mode list (i.e. a real connected panel).
        let new_secondary = displays
            .iter()
            .find(|d| d.name != primary_name && (!d.modes.is_empty() || d.active))
            .map(|d| d.name.clone());

        // Decide the target mode.
        let target = match (&new_secondary, prev_secondary.as_ref()) {
            // No external display connected.
            (None, _) => DisplayMode::SingleInternal,
            // A new (or changed) external display → reset to Mirror (decision #4).
            (Some(s), prev) if Some(s) != prev => DisplayMode::Mirror,
            // Same external display still present → keep the user's current mode,
            // unless we were in SingleInternal (shouldn't happen) → Mirror.
            (Some(_), _) => {
                if prev_mode == DisplayMode::SingleInternal {
                    DisplayMode::Mirror
                } else {
                    prev_mode
                }
            }
        };

        let topology_unchanged = new_secondary == prev_secondary && target == prev_mode;
        if topology_unchanged && !initial {
            return;
        }

        self.apply(&primary_name, new_secondary.clone(), target, displays)
            .await;
    }

    /// Apply `target` given the resolved primary/secondary and return by
    /// broadcasting the new state. Each branch is an idempotent sequence of
    /// output operations.
    async fn apply(
        &self,
        primary: &str,
        secondary: Option<String>,
        target: DisplayMode,
        displays: &[DisplayInfo],
    ) {
        let primary_info = displays.iter().find(|d| d.name == primary);
        let secondary_info = secondary
            .as_ref()
            .and_then(|s| displays.iter().find(|d| &d.name == s));

        // Resolve the mode we actually apply, and downgrade Mirror→ExternalOnly
        // if wl-mirror can't run so the invariant (one active output) still holds.
        let mut effective = target;

        match target {
            DisplayMode::SingleInternal => {
                self.mirror.stop().await;
                if let Err(e) = self.backend.enable_output(primary).await {
                    warn!(error = %e, "Failed to enable primary output");
                }
                self.restore_primary_native(primary).await;
                let _ = self
                    .backend
                    .map_pointer_to_output(POINTER_ALL_OUTPUTS)
                    .await;
                self.audio.restore().await;
            }
            DisplayMode::Mirror => {
                let Some(sec) = secondary.clone() else {
                    effective = DisplayMode::SingleInternal;
                    self.mirror.stop().await;
                    let _ = self.backend.enable_output(primary).await;
                    self.restore_primary_native(primary).await;
                    let _ = self
                        .backend
                        .map_pointer_to_output(POINTER_ALL_OUTPUTS)
                        .await;
                    self.audio.restore().await;
                    self.commit(primary, None, effective).await;
                    return;
                };
                // Enable both panels; drive the primary at the highest mutually
                // compatible mode so the mirror is clean and the TV upscales.
                // The external keeps its native mode and its own place in the
                // layout (overlapping outputs breaks sway's rendering under
                // screencopy). Instead, the pointer is confined to the primary
                // below so it can't reach the uninteractive wl-mirror surface.
                let _ = self.backend.enable_output(primary).await;
                let _ = self.backend.enable_output(&sec).await;
                if let (Some(p), Some(s)) = (primary_info, secondary_info)
                    && let Some(mode) = pick_mirror_mode(p, s)
                    && let Err(e) = self.backend.set_output_mode(primary, mode).await
                {
                    warn!(error = %e, "Failed to set primary mirror mode");
                }
                if self.mirror.start(primary).await {
                    // Pin the mirror window fullscreen onto the external output.
                    let criteria = format!("app_id=\"{WL_MIRROR_APP_ID}\"");
                    // wl-mirror maps its window shortly after spawn; give it a
                    // moment before we address it.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Err(e) = self
                        .backend
                        .move_to_output_fullscreen(&criteria, &sec)
                        .await
                    {
                        warn!(error = %e, "Failed to pin wl-mirror to external output");
                    }
                    // Confine the pointer to the primary so it can't wander onto
                    // the mirror surface, where clicks wouldn't reach the real
                    // content (issue #87).
                    let _ = self.backend.map_pointer_to_output(primary).await;
                    self.route_audio_external().await;
                } else {
                    // wl-mirror unavailable: fall back to external-only, which
                    // still keeps a single active output.
                    warn!("Mirroring unavailable; falling back to external-only");
                    effective = DisplayMode::ExternalOnly;
                    let _ = self
                        .backend
                        .map_pointer_to_output(POINTER_ALL_OUTPUTS)
                        .await;
                    self.apply_external_only(primary, &sec, secondary_info)
                        .await;
                }
            }
            DisplayMode::ExternalOnly => {
                let Some(sec) = secondary.clone() else {
                    effective = DisplayMode::SingleInternal;
                    self.mirror.stop().await;
                    let _ = self.backend.enable_output(primary).await;
                    self.restore_primary_native(primary).await;
                    let _ = self
                        .backend
                        .map_pointer_to_output(POINTER_ALL_OUTPUTS)
                        .await;
                    self.audio.restore().await;
                    self.commit(primary, None, effective).await;
                    return;
                };
                self.mirror.stop().await;
                // Only the external output is active now, so release any
                // pointer confinement from a prior mirror mode.
                let _ = self
                    .backend
                    .map_pointer_to_output(POINTER_ALL_OUTPUTS)
                    .await;
                self.apply_external_only(primary, &sec, secondary_info)
                    .await;
            }
        }

        self.commit(primary, secondary, effective).await;
    }

    /// Enable the external at its native mode, disable the primary, route audio.
    async fn apply_external_only(
        &self,
        primary: &str,
        secondary: &str,
        secondary_info: Option<&DisplayInfo>,
    ) {
        let _ = self.backend.enable_output(secondary).await;
        // Use the external's native/current mode; sway keeps its preferred mode
        // when we don't force one, so only set it if we know it explicitly.
        if let Some(mode) = secondary_info.and_then(|d| d.current_mode.or_else(|| highest(d))) {
            let _ = self.backend.set_output_mode(secondary, mode).await;
        }
        if let Err(e) = self.backend.disable_output(primary).await {
            warn!(error = %e, "Failed to disable primary output for external-only mode");
        }
        self.route_audio_external().await;
    }

    async fn route_audio_external(&self) {
        if self.mirror_audio {
            self.audio.route_to_external().await;
        }
    }

    /// Restore the primary to the mode captured at startup, undoing any
    /// mirror-driven mode change (issue #87). Called when the external display
    /// is disconnected and the primary becomes the sole output again.
    async fn restore_primary_native(&self, primary: &str) {
        let native = self.inner.lock().await.primary_native_mode;
        if let Some(mode) = native
            && let Err(e) = self.backend.set_output_mode(primary, mode).await
        {
            warn!(error = %e, "Failed to restore primary native mode");
        }
    }

    /// Persist the new state and broadcast it.
    async fn commit(&self, primary: &str, secondary: Option<String>, mode: DisplayMode) {
        let state = {
            let mut inner = self.inner.lock().await;
            inner.primary = Some(primary.to_string());
            inner.secondary = secondary;
            inner.mode = mode;
            inner.to_state()
        };
        info!(?mode, secondary = ?state.secondary, "Applied display arrangement");
        self.broadcast(state);
    }
}

/// Highest-resolution advertised mode of an output.
fn highest(d: &DisplayInfo) -> Option<VideoMode> {
    d.modes.iter().copied().max_by(|a, b| {
        a.area()
            .cmp(&b.area())
            .then(a.refresh_mhz.cmp(&b.refresh_mhz))
    })
}

#[async_trait]
impl DisplayController for DisplayManager {
    async fn state(&self) -> DisplayState {
        self.inner.lock().await.to_state()
    }

    async fn set_mode(&self, mode: DisplayMode) -> DisplayState {
        // Serialize against concurrent toggles/hotplugs so a burst of clicks
        // can't interleave and corrupt the arrangement.
        let _serial = self.apply_lock.lock().await;
        let displays = match self.backend.get_displays().await {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Failed to query displays for set_mode");
                return self.state().await;
            }
        };
        let (primary, secondary) = {
            let inner = self.inner.lock().await;
            (inner.primary.clone(), inner.secondary.clone())
        };
        let Some(primary) = primary else {
            return self.state().await;
        };
        // Mirror / ExternalOnly require an external display; ignore otherwise.
        if matches!(mode, DisplayMode::Mirror | DisplayMode::ExternalOnly) && secondary.is_none() {
            return self.state().await;
        }
        self.apply(&primary, secondary, mode, &displays).await;
        self.state().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use shepherd_host_api::HostResult;
    use std::sync::Mutex as StdMutex;

    /// Records the sway commands issued and serves a scripted display topology.
    #[derive(Default)]
    struct MockBackend {
        displays: StdMutex<Vec<DisplayInfo>>,
        ops: StdMutex<Vec<String>>,
    }

    impl MockBackend {
        fn set_displays(&self, d: Vec<DisplayInfo>) {
            *self.displays.lock().unwrap() = d;
        }
        fn ops(&self) -> Vec<String> {
            self.ops.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl OutputBackend for MockBackend {
        async fn get_displays(&self) -> HostResult<Vec<DisplayInfo>> {
            Ok(self.displays.lock().unwrap().clone())
        }
        async fn set_output_mode(&self, name: &str, mode: VideoMode) -> HostResult<()> {
            self.ops
                .lock()
                .unwrap()
                .push(format!("mode {name} {}x{}", mode.width, mode.height));
            Ok(())
        }
        async fn map_pointer_to_output(&self, output: &str) -> HostResult<()> {
            self.ops.lock().unwrap().push(format!("pointer {output}"));
            Ok(())
        }
        async fn set_output_scale(&self, name: &str, scale: f64) -> HostResult<()> {
            self.ops
                .lock()
                .unwrap()
                .push(format!("scale {name} {scale}"));
            Ok(())
        }
        async fn enable_output(&self, name: &str) -> HostResult<()> {
            self.ops.lock().unwrap().push(format!("enable {name}"));
            Ok(())
        }
        async fn disable_output(&self, name: &str) -> HostResult<()> {
            self.ops.lock().unwrap().push(format!("disable {name}"));
            Ok(())
        }
        async fn move_to_output_fullscreen(&self, _c: &str, output: &str) -> HostResult<()> {
            self.ops.lock().unwrap().push(format!("move {output}"));
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockMirror {
        events: StdMutex<Vec<String>>,
        available: bool,
    }
    #[async_trait]
    impl MirrorLauncher for MockMirror {
        async fn start(&self, src: &str) -> bool {
            self.events.lock().unwrap().push(format!("start {src}"));
            self.available
        }
        async fn stop(&self) {
            self.events.lock().unwrap().push("stop".into());
        }
    }

    #[derive(Default)]
    struct MockAudio {
        events: StdMutex<Vec<String>>,
    }
    #[async_trait]
    impl AudioRouter for MockAudio {
        async fn route_to_external(&self) {
            self.events.lock().unwrap().push("route".into());
        }
        async fn restore(&self) {
            self.events.lock().unwrap().push("restore".into());
        }
    }

    fn disp(name: &str, active: bool, modes: &[(u32, u32)]) -> DisplayInfo {
        DisplayInfo {
            name: name.into(),
            active,
            focused: false,
            make: None,
            model: None,
            current_mode: modes.first().map(|&(w, h)| VideoMode {
                width: w,
                height: h,
                refresh_mhz: 60000,
            }),
            modes: modes
                .iter()
                .map(|&(w, h)| VideoMode {
                    width: w,
                    height: h,
                    refresh_mhz: 60000,
                })
                .collect(),
        }
    }

    fn manager(
        backend: Arc<MockBackend>,
        mirror: Arc<MockMirror>,
        audio: Arc<MockAudio>,
    ) -> DisplayManager {
        // A real IpcServer isn't needed for logic: broadcast_event only pushes
        // onto a channel, so an unbound server (never `run()`) is fine.
        let ipc = Arc::new(IpcServer::new("/tmp/shepherd-display-test.sock"));
        let (tx, _rx) = broadcast::channel(16);
        DisplayManager::new(backend, mirror, audio, true, ipc, tx)
    }

    #[tokio::test]
    async fn single_internal_when_no_secondary() {
        let backend = Arc::new(MockBackend::default());
        backend.set_displays(vec![disp("eDP-1", true, &[(1280, 800)])]);
        let mirror = Arc::new(MockMirror::default());
        let audio = Arc::new(MockAudio::default());
        let mgr = manager(backend.clone(), mirror.clone(), audio.clone());
        mgr.initialize().await;
        assert_eq!(mgr.state().await.mode, DisplayMode::SingleInternal);
        assert_eq!(mgr.state().await.primary.as_deref(), Some("eDP-1"));
    }

    #[tokio::test]
    async fn connecting_secondary_enters_mirror() {
        let backend = Arc::new(MockBackend::default());
        backend.set_displays(vec![disp("eDP-1", true, &[(1920, 1080), (1280, 800)])]);
        let mirror = Arc::new(MockMirror {
            available: true,
            ..Default::default()
        });
        let audio = Arc::new(MockAudio::default());
        let mgr = manager(backend.clone(), mirror.clone(), audio.clone());
        mgr.initialize().await;
        // Plug in an external display sharing 1920x1080.
        backend.set_displays(vec![
            disp("eDP-1", true, &[(1920, 1080), (1280, 800)]),
            disp("HDMI-A-1", true, &[(1920, 1080)]),
        ]);
        mgr.on_output_changed().await;
        let st = mgr.state().await;
        assert_eq!(st.mode, DisplayMode::Mirror);
        assert_eq!(st.secondary.as_deref(), Some("HDMI-A-1"));
        assert!(
            mirror
                .events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "start eDP-1")
        );
        assert!(audio.events.lock().unwrap().iter().any(|e| e == "route"));
        // The primary was driven at the common 1920x1080 mode.
        assert!(backend.ops().iter().any(|o| o == "mode eDP-1 1920x1080"));
        // The pointer is confined to the primary so it can't wander onto the
        // mirror surface (issue #87).
        assert!(backend.ops().iter().any(|o| o == "pointer eDP-1"));
    }

    #[tokio::test]
    async fn toggle_to_external_only_disables_primary() {
        let backend = Arc::new(MockBackend::default());
        let both = vec![
            disp("eDP-1", true, &[(1920, 1080)]),
            disp("HDMI-A-1", true, &[(3840, 2160)]),
        ];
        backend.set_displays(both);
        let mirror = Arc::new(MockMirror {
            available: true,
            ..Default::default()
        });
        let audio = Arc::new(MockAudio::default());
        let mgr = manager(backend.clone(), mirror.clone(), audio.clone());
        mgr.initialize().await; // primary eDP-1, secondary HDMI → Mirror
        assert_eq!(mgr.state().await.mode, DisplayMode::Mirror);
        let st = mgr.set_mode(DisplayMode::ExternalOnly).await;
        assert_eq!(st.mode, DisplayMode::ExternalOnly);
        assert!(backend.ops().iter().any(|o| o == "disable eDP-1"));
        // Pointer confinement from mirror mode is released now that the primary
        // is gone and only the external is active.
        assert!(backend.ops().iter().any(|o| o == "pointer *"));
    }

    #[tokio::test]
    async fn disconnect_restores_primary_native_mode() {
        // Primary native mode is 1280x800; mirroring drives it to the common
        // 1280x720. Unplugging the external must put it back to 1280x800.
        let backend = Arc::new(MockBackend::default());
        backend.set_displays(vec![
            disp("eDP-1", true, &[(1280, 800), (1280, 720)]),
            disp("HDMI-A-1", true, &[(1280, 720)]),
        ]);
        let mirror = Arc::new(MockMirror {
            available: true,
            ..Default::default()
        });
        let audio = Arc::new(MockAudio::default());
        let mgr = manager(backend.clone(), mirror.clone(), audio.clone());
        mgr.initialize().await;
        assert_eq!(mgr.state().await.mode, DisplayMode::Mirror);
        assert!(backend.ops().iter().any(|o| o == "mode eDP-1 1280x720"));
        // Unplug the external.
        backend.set_displays(vec![disp("eDP-1", true, &[(1280, 800), (1280, 720)])]);
        mgr.on_output_changed().await;
        assert_eq!(mgr.state().await.mode, DisplayMode::SingleInternal);
        // The primary was restored to its captured native mode.
        assert!(backend.ops().iter().any(|o| o == "mode eDP-1 1280x800"));
    }

    #[tokio::test]
    async fn mirror_falls_back_to_external_when_wl_mirror_missing() {
        let backend = Arc::new(MockBackend::default());
        backend.set_displays(vec![
            disp("eDP-1", true, &[(1920, 1080)]),
            disp("HDMI-A-1", true, &[(1920, 1080)]),
        ]);
        let mirror = Arc::new(MockMirror {
            available: false,
            ..Default::default()
        });
        let audio = Arc::new(MockAudio::default());
        let mgr = manager(backend.clone(), mirror.clone(), audio.clone());
        mgr.initialize().await;
        assert_eq!(mgr.state().await.mode, DisplayMode::ExternalOnly);
        assert!(backend.ops().iter().any(|o| o == "disable eDP-1"));
    }
}
