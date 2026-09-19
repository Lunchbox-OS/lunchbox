//! Linux volume control implementation
//!
//! Provides volume control with auto-detection of sound systems:
//! - PipeWire (via `wpctl`)
//! - PulseAudio (via `pactl`)
//! - ALSA (via `amixer`)

use crate::helpers;
use async_trait::async_trait;
use lunchbox_host_api::{
    AudioSnapshot, VolumeCapabilities, VolumeController, VolumeError, VolumeResult, VolumeStatus,
};

use crate::audio;
use tracing::{debug, info, warn};

/// Detected sound backend
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundBackend {
    /// PipeWire with WirePlumber
    PipeWire,
    /// PulseAudio
    PulseAudio,
    /// ALSA (direct)
    Alsa,
}

impl SoundBackend {
    /// Detect the best available sound backend
    pub fn detect() -> Option<Self> {
        // Try PipeWire first (modern systems)
        if Self::is_pipewire_available() {
            info!("Detected PipeWire sound backend");
            return Some(Self::PipeWire);
        }

        // Try PulseAudio
        if Self::is_pulseaudio_available() {
            info!("Detected PulseAudio sound backend");
            return Some(Self::PulseAudio);
        }

        // Try ALSA as fallback
        if Self::is_alsa_available() {
            info!("Detected ALSA sound backend");
            return Some(Self::Alsa);
        }

        warn!("No sound backend detected");
        None
    }

    fn is_pipewire_available() -> bool {
        // Check if wpctl is available and can communicate with PipeWire
        helpers::command("wpctl")
            .args(["status"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn is_pulseaudio_available() -> bool {
        // Check if pactl is available and server is running
        helpers::command("pactl")
            .args(["info"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn is_alsa_available() -> bool {
        // Check if amixer is available
        helpers::command("amixer")
            .args(["sget", "Master"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::PipeWire => "pipewire",
            Self::PulseAudio => "pulseaudio",
            Self::Alsa => "alsa",
        }
    }
}

/// Linux volume controller with auto-detection
pub struct LinuxVolumeController {
    capabilities: VolumeCapabilities,
    backend: Option<SoundBackend>,
}

impl LinuxVolumeController {
    /// Create a new volume controller with auto-detection
    pub fn new() -> Self {
        let backend = SoundBackend::detect();

        let capabilities = VolumeCapabilities {
            available: backend.is_some(),
            backend: backend.map(|b| b.name().to_string()),
            can_mute: backend.is_some(),
            max_volume: 100,
        };

        Self {
            capabilities,
            backend,
        }
    }

    /// Get volume status via PipeWire
    fn get_status_pipewire() -> VolumeResult<VolumeStatus> {
        let stdout = run_reading("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
        parse_wpctl_volume(&stdout).ok_or_else(|| unreadable("wpctl get-volume", &stdout))
    }

    /// Get volume status via PulseAudio
    fn get_status_pulseaudio() -> VolumeResult<VolumeStatus> {
        // Two commands, and both have to answer: a mute state we could not read
        // is not "not muted". Reporting sound as on when it is off is the wrong
        // way round to be wrong.
        let vol = run_reading("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])?;
        let percent =
            parse_pactl_volume(&vol).ok_or_else(|| unreadable("pactl get-sink-volume", &vol))?;
        let mute = run_reading("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])?;
        let muted =
            parse_pactl_mute(&mute).ok_or_else(|| unreadable("pactl get-sink-mute", &mute))?;
        Ok(VolumeStatus { percent, muted })
    }

    /// Get volume status via ALSA
    fn get_status_alsa() -> VolumeResult<VolumeStatus> {
        let stdout = run_reading("amixer", &["sget", "Master"])?;
        parse_amixer_status(&stdout).ok_or_else(|| unreadable("amixer sget Master", &stdout))
    }

    /// Set volume via PipeWire
    fn set_volume_pipewire(percent: u8) -> VolumeResult<()> {
        let volume = format!("{}%", percent);
        helpers::command("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &volume])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Set volume via PulseAudio
    fn set_volume_pulseaudio(percent: u8) -> VolumeResult<()> {
        helpers::command("pactl")
            .args([
                "set-sink-volume",
                "@DEFAULT_SINK@",
                &format!("{}%", percent),
            ])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Set volume via ALSA
    fn set_volume_alsa(percent: u8) -> VolumeResult<()> {
        helpers::command("amixer")
            .args(["sset", "Master", &format!("{}%", percent)])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Toggle mute via PipeWire
    fn toggle_mute_pipewire() -> VolumeResult<()> {
        helpers::command("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Toggle mute via PulseAudio
    fn toggle_mute_pulseaudio() -> VolumeResult<()> {
        helpers::command("pactl")
            .args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Toggle mute via ALSA
    fn toggle_mute_alsa() -> VolumeResult<()> {
        helpers::command("amixer")
            .args(["sset", "Master", "toggle"])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Set mute state via PipeWire
    fn set_mute_pipewire(muted: bool) -> VolumeResult<()> {
        let state = if muted { "1" } else { "0" };
        helpers::command("wpctl")
            .args(["set-mute", "@DEFAULT_AUDIO_SINK@", state])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Set mute state via PulseAudio
    fn set_mute_pulseaudio(muted: bool) -> VolumeResult<()> {
        let state = if muted { "1" } else { "0" };
        helpers::command("pactl")
            .args(["set-sink-mute", "@DEFAULT_SINK@", state])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }

    /// Set mute state via ALSA
    fn set_mute_alsa(muted: bool) -> VolumeResult<()> {
        let state = if muted { "mute" } else { "unmute" };
        helpers::command("amixer")
            .args(["sset", "Master", state])
            .status()
            .map_err(|e| VolumeError::Backend(e.to_string()))?;
        Ok(())
    }
}

impl Default for LinuxVolumeController {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VolumeController for LinuxVolumeController {
    fn capabilities(&self) -> &VolumeCapabilities {
        &self.capabilities
    }

    async fn get_status(&self) -> VolumeResult<VolumeStatus> {
        match self.backend {
            Some(SoundBackend::PipeWire) => Self::get_status_pipewire(),
            Some(SoundBackend::PulseAudio) => Self::get_status_pulseaudio(),
            Some(SoundBackend::Alsa) => Self::get_status_alsa(),
            None => Err(VolumeError::NotAvailable(
                "No sound backend available".into(),
            )),
        }
    }

    async fn set_volume(&self, percent: u8) -> VolumeResult<()> {
        if percent > self.capabilities.max_volume {
            return Err(VolumeError::OutOfRange(percent));
        }

        match self.backend {
            Some(SoundBackend::PipeWire) => Self::set_volume_pipewire(percent),
            Some(SoundBackend::PulseAudio) => Self::set_volume_pulseaudio(percent),
            Some(SoundBackend::Alsa) => Self::set_volume_alsa(percent),
            None => Err(VolumeError::NotAvailable(
                "No sound backend available".into(),
            )),
        }
    }

    async fn volume_up(&self, step: u8) -> VolumeResult<()> {
        let current = self.get_status().await?;
        let new_volume = current
            .percent
            .saturating_add(step)
            .min(self.capabilities.max_volume);
        self.set_volume(new_volume).await
    }

    async fn volume_down(&self, step: u8) -> VolumeResult<()> {
        let current = self.get_status().await?;
        let new_volume = current.percent.saturating_sub(step);
        self.set_volume(new_volume).await
    }

    async fn toggle_mute(&self) -> VolumeResult<()> {
        match self.backend {
            Some(SoundBackend::PipeWire) => Self::toggle_mute_pipewire(),
            Some(SoundBackend::PulseAudio) => Self::toggle_mute_pulseaudio(),
            Some(SoundBackend::Alsa) => Self::toggle_mute_alsa(),
            None => Err(VolumeError::NotAvailable(
                "No sound backend available".into(),
            )),
        }
    }

    async fn set_mute(&self, muted: bool) -> VolumeResult<()> {
        match self.backend {
            Some(SoundBackend::PipeWire) => Self::set_mute_pipewire(muted),
            Some(SoundBackend::PulseAudio) => Self::set_mute_pulseaudio(muted),
            Some(SoundBackend::Alsa) => Self::set_mute_alsa(muted),
            None => Err(VolumeError::NotAvailable(
                "No sound backend available".into(),
            )),
        }
    }

    /// Only PipeWire can name its outputs. PulseAudio and ALSA fall through to
    /// the trait default and keep behaving as a single anonymous output.
    async fn current_output(&self) -> Option<lunchbox_api::AudioOutput> {
        if self.backend != Some(SoundBackend::PipeWire) {
            return None;
        }
        // This returns `Option`, so a failed read cannot be told apart from "no
        // active output" by the caller. Anything that must not confuse the two
        // (the restriction lookup, above all) goes through `observe` instead.
        let topo = audio::dump()
            .await
            .inspect_err(|e| warn!(error = %e, "Could not read the audio topology"))
            .ok()?;
        topo.current_output().map(to_api_output)
    }

    async fn select_output(&self, output_key: &str) -> VolumeResult<()> {
        if self.backend != Some(SoundBackend::PipeWire) {
            return Err(VolumeError::NotAvailable(
                "choosing an output needs PipeWire".into(),
            ));
        }
        let topo = audio::dump()
            .await
            .map_err(|e| VolumeError::Backend(format!("could not read the audio topology: {e}")))?;
        let output = topo.output_by_key(output_key).ok_or_else(|| {
            // A remembered row for a device that is not plugged in right now
            // lands here, which is the common case and not an internal error.
            VolumeError::NotAvailable(format!("audio output is not connected: {output_key}"))
        })?;
        if !output.usable {
            return Err(VolumeError::NotAvailable(format!(
                "nothing is plugged into this output: {output_key}"
            )));
        }
        let id = topo.node_id_of(output).ok_or_else(|| {
            VolumeError::Backend(format!("no sink node named {}", output.node_name))
        })?;
        if audio::set_default_sink(id).await {
            info!(output = %output_key, sink = %output.node_name, "Selected audio output");
            Ok(())
        } else {
            Err(VolumeError::Backend(format!(
                "wpctl set-default failed for {}",
                output.node_name
            )))
        }
    }

    /// One `pw-dump` already describes the whole topology — every sink, its
    /// volume, and which one is default — so the entire snapshot costs a single
    /// process spawn and nothing in it can disagree with anything else.
    async fn observe(&self) -> VolumeResult<AudioSnapshot> {
        if self.backend != Some(SoundBackend::PipeWire) {
            return Ok(AudioSnapshot {
                status: self.get_status().await?,
                ..Default::default()
            });
        }
        // The backend was detected as PipeWire, so a read that fails is a
        // failure, not a machine with no sinks. Reporting the empty snapshot
        // here is what made a transient `pw-dump` failure indistinguishable
        // from "every device was unplugged at once": it relaxed the active
        // output's cap back to the global one and greyed out every row in both
        // UIs. Fail instead, and let each caller decide what it knows.
        let topo = audio::dump()
            .await
            .map_err(|e| VolumeError::Backend(format!("could not read the audio topology: {e}")))?;
        // `usable` is false only when jack detection explicitly says nothing is
        // plugged into the port; cards without jack detection report "unknown",
        // which has to count as usable or every output on such a host vanishes.
        let outputs: Vec<_> = topo
            .outputs
            .iter()
            .filter(|o| o.usable)
            .map(to_api_output)
            .collect();
        match topo.current_output() {
            Some(out) => Ok(AudioSnapshot {
                status: VolumeStatus {
                    percent: out.volume_percent,
                    muted: out.muted,
                },
                active: Some(to_api_output(out)),
                outputs,
            }),
            // No default sink resolved (PipeWire still starting, or no sinks at
            // all) — fall back rather than reporting a bogus zero.
            None => Ok(AudioSnapshot {
                status: self.get_status().await?,
                active: None,
                outputs,
            }),
        }
    }
}

/// Run a reading command and hand back its stdout, refusing anything that did
/// not actually run.
///
/// The exit status used to be ignored on all three backends, so a command that
/// failed contributed an empty string to a parser that answered `0` — reported
/// as 0 %, which is indistinguishable from genuine silence and is a perfectly
/// plausible reading to act on.
fn run_reading(program: &str, args: &[&str]) -> VolumeResult<String> {
    let output = helpers::command(program)
        .args(args)
        .output()
        .map_err(|e| VolumeError::Backend(format!("{program}: {e}")))?;
    if !output.status.success() {
        return Err(VolumeError::Backend(format!(
            "{program} {} exited {}",
            args.join(" "),
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    debug!(%program, output = stdout.trim(), "read volume status");
    Ok(stdout)
}

/// The error for output that ran but did not say anything we understand.
///
/// Separate from a failed spawn on purpose: this one means the tool changed its
/// format under us, which is the standing risk of reading a CLI meant for people.
fn unreadable(what: &str, stdout: &str) -> VolumeError {
    VolumeError::Backend(format!(
        "could not read the volume from `{what}` output: {:?}",
        stdout.trim()
    ))
}

/// `"Volume: 0.50"` / `"Volume: 0.50 [MUTED]"` -> 50 %, muted.
///
/// `None` when the line is not that shape at all, so the caller reports a
/// failure to read rather than a reading of zero.
pub(crate) fn parse_wpctl_volume(stdout: &str) -> Option<VolumeStatus> {
    let percent = stdout
        .split(':')
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<f32>()
        .ok()?;
    if !percent.is_finite() || percent < 0.0 {
        return None;
    }
    Some(VolumeStatus {
        percent: (percent * 100.0).round().min(u8::MAX as f32) as u8,
        muted: stdout.contains("[MUTED]"),
    })
}

/// `"Volume: front-left: 65536 / 100% / -0.00 dB, ..."` -> 100 %.
pub(crate) fn parse_pactl_volume(stdout: &str) -> Option<u8> {
    stdout
        .split('/')
        .nth(1)?
        .trim()
        .trim_end_matches('%')
        .parse::<u8>()
        .ok()
}

/// `"Mute: yes"` / `"Mute: no"` -> muted. Anything else is unreadable, rather
/// than "not muted" — the absence of the word `yes` is not evidence.
pub(crate) fn parse_pactl_mute(stdout: &str) -> Option<bool> {
    let value = stdout.split(':').nth(1)?.trim();
    match value {
        v if v.starts_with("yes") => Some(true),
        v if v.starts_with("no") => Some(false),
        _ => None,
    }
}

/// `"Front Left: Playback 65536 [100%] [on]"` -> 100 %, unmuted.
pub(crate) fn parse_amixer_status(stdout: &str) -> Option<VolumeStatus> {
    let line = stdout
        .lines()
        .find(|l| l.contains("Playback") && l.contains('%'))?;
    let start = line.find('[')?;
    let end = line[start..].find('%')?;
    let percent = line[start + 1..start + end].parse::<u8>().ok()?;
    Some(VolumeStatus {
        percent,
        muted: line.contains("[off]"),
    })
}

/// Project the host-side topology entry onto the wire type. Only the stable key,
/// a display label, and the advisory kind cross the boundary; node names and
/// object ids stay inside the host adapter.
pub(crate) fn to_api_output(o: &audio::AudioOutput) -> lunchbox_api::AudioOutput {
    lunchbox_api::AudioOutput {
        key: o.key(),
        description: o.description.clone(),
        kind: match o.kind {
            audio::AudioOutputKind::Speakers => lunchbox_api::AudioOutputKind::Speakers,
            audio::AudioOutputKind::Headphones => lunchbox_api::AudioOutputKind::Headphones,
            audio::AudioOutputKind::Hdmi => lunchbox_api::AudioOutputKind::Hdmi,
            audio::AudioOutputKind::Digital => lunchbox_api::AudioOutputKind::Digital,
            audio::AudioOutputKind::LineOut => lunchbox_api::AudioOutputKind::LineOut,
            audio::AudioOutputKind::Bluetooth => lunchbox_api::AudioOutputKind::Bluetooth,
            audio::AudioOutputKind::Unknown => lunchbox_api::AudioOutputKind::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_name() {
        assert_eq!(SoundBackend::PipeWire.name(), "pipewire");
        assert_eq!(SoundBackend::PulseAudio.name(), "pulseaudio");
        assert_eq!(SoundBackend::Alsa.name(), "alsa");
    }

    #[test]
    fn wpctl_output_is_read() {
        assert_eq!(
            parse_wpctl_volume("Volume: 0.50\n"),
            Some(VolumeStatus {
                percent: 50,
                muted: false
            })
        );
        assert_eq!(
            parse_wpctl_volume("Volume: 0.30 [MUTED]\n"),
            Some(VolumeStatus {
                percent: 30,
                muted: true
            })
        );
        // Over-unity volumes are real; PipeWire allows boosting past 1.0.
        assert_eq!(parse_wpctl_volume("Volume: 1.40\n").unwrap().percent, 140);
    }

    /// The whole point: output we cannot read must not become a reading.
    ///
    /// Every one of these used to answer `0%, not muted` — a plausible value a
    /// parent could act on, produced by a command that told us nothing. An empty
    /// string is what a failed `wpctl` actually contributed, because the exit
    /// status was not checked either.
    #[test]
    fn unreadable_wpctl_output_is_not_a_reading_of_zero() {
        for junk in [
            "",
            "\n",
            "wpctl: command not found\n",
            "Volume:\n",
            "Volume: not-a-number\n",
            "Node 52 not found\n",
        ] {
            assert_eq!(
                parse_wpctl_volume(junk),
                None,
                "{junk:?} must read as unreadable, not as 0%"
            );
        }
    }

    #[test]
    fn pactl_output_is_read() {
        assert_eq!(
            parse_pactl_volume(
                "Volume: front-left: 65536 /  100% / -0.00 dB,   front-right: 65536 /  100%\n"
            ),
            Some(100)
        );
        assert_eq!(parse_pactl_mute("Mute: yes\n"), Some(true));
        assert_eq!(parse_pactl_mute("Mute: no\n"), Some(false));
    }

    /// A mute state we could not read is not "not muted": that reports sound as
    /// on when it may be off, which is the wrong way round to be wrong.
    #[test]
    fn unreadable_pactl_output_is_not_a_reading() {
        for junk in ["", "\n", "Failure: No such entity\n"] {
            assert_eq!(parse_pactl_volume(junk), None, "{junk:?} volume");
            assert_eq!(parse_pactl_mute(junk), None, "{junk:?} mute");
        }
        // Absence of the word "yes" used to be taken as evidence of not-muted.
        assert_eq!(parse_pactl_mute("Mute: unknown\n"), None);
    }

    #[test]
    fn amixer_output_is_read() {
        let out = "Simple mixer control 'Master',0\n  Capabilities: pvolume pswitch\n  \
                   Front Left: Playback 65536 [100%] [0.00dB] [on]\n";
        assert_eq!(
            parse_amixer_status(out),
            Some(VolumeStatus {
                percent: 100,
                muted: false
            })
        );
        let muted = "  Front Left: Playback 0 [0%] [-99.99dB] [off]\n";
        assert_eq!(
            parse_amixer_status(muted),
            Some(VolumeStatus {
                percent: 0,
                muted: true
            })
        );
    }

    /// A genuine 0% must still read as 0% — the fix is about telling the two
    /// apart, not about treating zero as suspicious.
    #[test]
    fn unreadable_amixer_output_is_not_a_reading_of_zero() {
        for junk in [
            "",
            "amixer: Unable to find simple control 'Master',0\n",
            "  Capabilities: pvolume pswitch\n",
        ] {
            assert_eq!(parse_amixer_status(junk), None, "{junk:?}");
        }
    }
}
