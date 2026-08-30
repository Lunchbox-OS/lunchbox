//! PipeWire audio topology: enumerate output devices and identify the active one.
//!
//! Two consumers share this parser:
//! - [`crate::audio_route`], which switches the default sink while docked (issue #87).
//! - The volume path (issue #124), which needs to know *which* output is selected
//!   so the displayed volume stops going stale when it changes.
//!
//! # Identifying an output
//!
//! The key is `(device.name, route.name)` — deliberately the same key WirePlumber
//! itself uses to persist per-route volume (`state-routes.lua` writes
//! `<device.name>:<direction>:<route.name>`). Keying the same way means our notion
//! of "an output" can never disagree with the volume PipeWire remembers for it.
//!
//! `device.name` rather than `node.name` because the node name embeds the card
//! *profile* and changes when the profile does. The route is what distinguishes
//! headphones from speakers: on an analog jack both live on one sink node, and only
//! the active route tells them apart.
//!
//! Numeric object ids are never used as identity — PipeWire recycles them across
//! restarts and re-plugs.

use crate::helpers;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::warn;

/// A PipeWire `Audio/Sink` node discovered from `pw-dump`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkNode {
    pub id: u32,
    pub name: String,
    /// True when the node looks like an HDMI/DisplayPort output.
    pub is_external_video: bool,
}

/// What kind of thing an output is. Advisory only — it drives presentation, never
/// policy, because it cannot be determined for every device (see [`classify`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioOutputKind {
    Speakers,
    Headphones,
    Hdmi,
    Digital,
    LineOut,
    Bluetooth,
    Unknown,
}

impl AudioOutputKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Speakers => "speakers",
            Self::Headphones => "headphones",
            Self::Hdmi => "hdmi",
            Self::Digital => "digital",
            Self::LineOut => "line_out",
            Self::Bluetooth => "bluetooth",
            Self::Unknown => "unknown",
        }
    }
}

/// One selectable audio output: a sink node together with its active output route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutput {
    /// Stable half of the identity key, e.g. `alsa_card.pci-0000_00_1b.0`.
    pub device_name: String,
    /// Discriminating half: the active output route, e.g. `analog-output-headphones`.
    /// `None` for devices with no route concept (Bluetooth).
    pub route_name: Option<String>,
    /// The sink the volume commands actually land on. Re-resolved from each dump;
    /// never cached as a numeric id.
    pub node_name: String,
    /// Human-readable label. Localized and mutable — display only, never a key.
    pub description: String,
    pub kind: AudioOutputKind,
    /// The sink's own volume, 0-100. Read from the same dump as the identity, so
    /// the pair is always self-consistent.
    pub volume_percent: u8,
    pub muted: bool,
    /// False only when the route reports `available == "no"` (jack detection says
    /// nothing is plugged in). Cards without jack detection report `unknown`, which
    /// must count as usable — otherwise every output on such a host disappears.
    pub usable: bool,
}

impl AudioOutput {
    /// The identity key, matching WirePlumber's own `default-routes` key format.
    pub fn key(&self) -> String {
        match &self.route_name {
            Some(r) => format!("{}:output:{}", self.device_name, r),
            None => format!("{}:output", self.device_name),
        }
    }
}

/// Everything one `pw-dump` tells us.
#[derive(Debug, Clone, Default)]
pub struct AudioTopology {
    pub sinks: Vec<SinkNode>,
    /// `node.name` of the current default sink, from PipeWire's metadata.
    pub default_sink: Option<String>,
    pub outputs: Vec<AudioOutput>,
}

impl AudioTopology {
    /// The output the default sink currently resolves to.
    pub fn current_output(&self) -> Option<&AudioOutput> {
        let default = self.default_sink.as_deref()?;
        self.outputs.iter().find(|o| o.node_name == default)
    }

    /// The output with this identity key, if it is present in this dump.
    pub fn output_by_key(&self, key: &str) -> Option<&AudioOutput> {
        self.outputs.iter().find(|o| o.key() == key)
    }

    /// The live node id for an output, for the one `wpctl` call that needs one.
    ///
    /// Resolved from the dump it is used with and never stored: PipeWire recycles
    /// object ids across restarts and re-plugs, so an id kept across either
    /// addresses a different node or nothing at all.
    pub fn node_id_of(&self, output: &AudioOutput) -> Option<u32> {
        self.sinks
            .iter()
            .find(|s| s.name == output.node_name)
            .map(|s| s.id)
    }
}

// ---------------------------------------------------------------- raw pw-dump

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
    props: HashMap<String, serde_json::Value>,
    #[serde(default)]
    params: RawParams,
}

#[derive(Debug, Default, Deserialize)]
struct RawParams {
    /// Currently-active routes. `EnumRoute` (everything the hardware *could* have)
    /// is deliberately ignored: only the live route identifies the output in use.
    #[serde(rename = "Route", default)]
    route: Vec<RawRoute>,
    #[serde(rename = "Props", default)]
    props: Vec<RawProps>,
}

#[derive(Debug, Default, Deserialize)]
struct RawProps {
    /// Per-channel volumes, cubic-scaled the way PipeWire stores them.
    #[serde(default)]
    #[serde(rename = "channelVolumes")]
    channel_volumes: Vec<f64>,
    #[serde(default)]
    volume: Option<f64>,
    #[serde(default)]
    mute: bool,
}

/// PipeWire stores volume as the cube of the fraction the UI shows, which is why
/// `wpctl get-volume` prints `cbrt(channelVolumes)`. Match that exactly so our
/// reading never disagrees with the rest of the system by a rounding step.
fn cubic_to_percent(props: &RawProps) -> u8 {
    let raw = props
        .channel_volumes
        .iter()
        .cloned()
        .fold(f64::NAN, f64::max);
    let raw = if raw.is_nan() {
        props.volume.unwrap_or(0.0)
    } else {
        raw
    };
    (raw.max(0.0).cbrt() * 100.0).round().clamp(0.0, 255.0) as u8
}

#[derive(Debug, Deserialize)]
struct RawRoute {
    #[serde(default)]
    direction: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    available: Option<String>,
    /// Card profile device index, matching a sink node's `card.profile.device`.
    #[serde(default)]
    device: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RawMetadataEntry {
    #[serde(default)]
    key: String,
    #[serde(default)]
    value: serde_json::Value,
}

fn prop_str<'a>(props: &'a HashMap<String, serde_json::Value>, key: &str) -> Option<&'a str> {
    props.get(key).and_then(|v| v.as_str())
}

fn prop_i64(props: &HashMap<String, serde_json::Value>, key: &str) -> Option<i64> {
    props.get(key).and_then(|v| v.as_i64())
}

// ---------------------------------------------------------------- classification

/// Map an ALSA card-profile route name to a kind.
///
/// The vocabulary is closed and shipped on disk in
/// `/usr/share/alsa-card-profile/mixer/paths/`. Trailing digits are stripped first
/// so `analog-output-headphones-2` and `hdmi-output-7` fold onto their stems.
/// Returns `None` for the generic ports (`analog-output`, `analog-output-mono`,
/// the `audigy-*` family), which genuinely carry no information — a plain USB
/// interface reports exactly that.
fn route_kind(route: &str) -> Option<AudioOutputKind> {
    let mut n = route.to_ascii_lowercase();
    while n.ends_with(|c: char| c.is_ascii_digit()) {
        n.pop();
    }
    let n = n.trim_end_matches('-');
    if n.contains("headphone") || n.contains("headset") || n.contains("chat") {
        Some(AudioOutputKind::Headphones)
    } else if n.contains("speaker") {
        Some(AudioOutputKind::Speakers)
    } else if n.contains("hdmi") || n.contains("displayport") || n.contains("display-port") {
        Some(AudioOutputKind::Hdmi)
    } else if n.contains("iec958") || n.contains("spdif") {
        Some(AudioOutputKind::Digital)
    } else if n.contains("lineout") || n.contains("line-out") {
        Some(AudioOutputKind::LineOut)
    } else {
        None
    }
}

/// Classify an output. Route name wins (it is the only *per-port* signal), then
/// transport, then the card-level `device.form-factor`.
///
/// `device.form-factor` is consulted last and `internal` is explicitly not a
/// classification: udev sets it for anything on the internal PCI bus
/// (`78-sound-card.rules`), so on a laptop it is always `internal` no matter which
/// port is live. That same rules file derives `headphone`/`headset`/`speaker` from
/// a substring match on the product model name, so it is a hint of the same class
/// as our own matching rather than authoritative metadata.
fn classify(
    device_props: &HashMap<String, serde_json::Value>,
    route: Option<&str>,
) -> AudioOutputKind {
    if let Some(k) = route.and_then(route_kind) {
        return k;
    }
    if prop_str(device_props, "device.api") == Some("bluez5") {
        return AudioOutputKind::Bluetooth;
    }
    match prop_str(device_props, "device.form-factor") {
        Some("headphone") | Some("headset") | Some("hands-free") => AudioOutputKind::Headphones,
        Some("speaker") => AudioOutputKind::Speakers,
        Some("tv") => AudioOutputKind::Hdmi,
        _ => AudioOutputKind::Unknown,
    }
}

/// True if any of the sink's identifying strings marks it as HDMI/DisplayPort.
fn looks_external_video(name: &str, alsa_path: &str) -> bool {
    let hay = format!("{name} {alsa_path}").to_ascii_lowercase();
    hay.contains("hdmi") || hay.contains("displayport") || hay.contains("display-port")
}

// ---------------------------------------------------------------- parsing

/// Parse `pw-dump` JSON into the audio topology.
pub fn parse_pw_dump(raw: &[u8]) -> AudioTopology {
    let objs: Vec<RawPwObject> = match serde_json::from_slice(raw) {
        Ok(o) => o,
        Err(e) => {
            warn!(error = %e, "Failed to parse pw-dump output");
            return AudioTopology::default();
        }
    };

    let mut default_sink = None;
    let mut devices: HashMap<u32, &RawPwInfo> = HashMap::new();

    for obj in &objs {
        if obj.obj_type.ends_with("Metadata") {
            for m in &obj.metadata {
                if m.key == "default.audio.sink"
                    && let Some(name) = m.value.get("name").and_then(|v| v.as_str())
                {
                    default_sink = Some(name.to_string());
                }
            }
        }
        if let (Some(id), Some(info)) = (obj.id, obj.info.as_ref())
            && prop_str(&info.props, "media.class") == Some("Audio/Device")
        {
            devices.insert(id, info);
        }
    }

    let mut sinks = Vec::new();
    let mut outputs = Vec::new();

    for obj in &objs {
        let (Some(id), Some(info)) = (obj.id, obj.info.as_ref()) else {
            continue;
        };
        let props = &info.props;
        if prop_str(props, "media.class") != Some("Audio/Sink") {
            continue;
        }

        let name = prop_str(props, "node.name").unwrap_or("").to_string();
        let alsa_path = prop_str(props, "api.alsa.path").unwrap_or("");
        let description = prop_str(props, "node.description").unwrap_or("");
        sinks.push(SinkNode {
            id,
            is_external_video: looks_external_video(&format!("{name} {description}"), alsa_path),
            name: name.clone(),
        });

        // Join to the parent device and pick the active output route for this
        // node's card-profile device index. A node whose device is missing (or a
        // device with no routes, as Bluetooth) still yields an output — it just
        // has no route half to its key.
        let device_info = prop_i64(props, "device.id")
            .and_then(|d| u32::try_from(d).ok())
            .and_then(|d| devices.get(&d).copied());
        let device_props = device_info.map(|i| &i.props);
        let card_device = prop_i64(props, "card.profile.device");
        let route = device_info.and_then(|i| {
            i.params
                .route
                .iter()
                .find(|r| r.direction == "Output" && r.device == card_device)
        });

        let empty = HashMap::new();
        let dprops = device_props.unwrap_or(&empty);
        // Fall back to the node name so an output is never keyless; a sink with no
        // parent device is unusual but must still be addressable.
        let device_name = prop_str(dprops, "device.name").unwrap_or(&name).to_string();
        let route_name = route.map(|r| r.name.clone()).filter(|r| !r.is_empty());

        let node_props = info.params.props.first();
        outputs.push(AudioOutput {
            kind: classify(dprops, route_name.as_deref()),
            volume_percent: node_props.map(cubic_to_percent).unwrap_or(0),
            muted: node_props.map(|p| p.mute).unwrap_or(false),
            // Only an explicit "no" means unplugged. "unknown" is what cards
            // without jack detection report, and both dev-host cards do.
            usable: route.and_then(|r| r.available.as_deref()) != Some("no"),
            device_name,
            route_name,
            description: description.to_string(),
            node_name: name,
        });
    }

    AudioTopology {
        sinks,
        default_sink,
        outputs,
    }
}

/// Why a `pw-dump` read produced no topology.
///
/// The two cases have to stay apart. Collapsing them is what let a failed read
/// look exactly like a host with no sinks: the caller cannot tell "there is no
/// PipeWire here" from "PipeWire is here and I could not read it", and the
/// second one must never be answered with an empty device list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DumpError {
    /// `pw-dump` could not be spawned — it is not installed. Expected on a
    /// non-PipeWire host, where it says nothing is wrong.
    Unavailable,
    /// `pw-dump` ran and exited non-zero. On a host already detected as
    /// PipeWire this is a real failure to read, not an absence of devices.
    Failed,
}

impl std::fmt::Display for DumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => write!(f, "pw-dump is not installed"),
            Self::Failed => write!(f, "pw-dump exited non-zero"),
        }
    }
}

/// Run `pw-dump` and parse it.
///
/// Returns [`DumpError`] rather than `None` so a caller on a PipeWire host can
/// refuse to treat a failed read as an empty topology.
pub async fn dump() -> Result<AudioTopology, DumpError> {
    let out = tokio::process::Command::new(helpers::resolve("pw-dump"))
        .output()
        .await
        .map_err(|_| DumpError::Unavailable)?;
    if !out.status.success() {
        return Err(DumpError::Failed);
    }
    Ok(parse_pw_dump(&out.stdout))
}

/// Point PipeWire's default sink at a node. `id` must come from the same dump it
/// is used with — see [`AudioTopology::node_id_of`].
pub async fn set_default_sink(id: u32) -> bool {
    tokio::process::Command::new(helpers::resolve("wpctl"))
        .args(["set-default", &id.to_string()])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modelled on a real `pw-dump` from a host with the built-in analog card and
    /// a passed-through Focusrite Scarlett 2i2, with the USB interface selected as
    /// the default sink — the arrangement that exposed both of the classification
    /// gaps this module has to tolerate.
    const TWO_DEVICE_DUMP: &[u8] = br#"[
      {"id":51,"type":"PipeWire:Interface:Device","info":{
        "props":{
          "media.class":"Audio/Device",
          "device.name":"alsa_card.pci-0000_00_1b.0",
          "device.description":"Built-in Audio",
          "device.form-factor":"internal",
          "device.bus":"pci",
          "device.api":"alsa"},
        "params":{"Route":[
          {"index":1,"direction":"Output","name":"analog-output-lineout",
           "available":"unknown","device":3}]}}},
      {"id":52,"type":"PipeWire:Interface:Node","info":{
        "props":{
          "media.class":"Audio/Sink",
          "node.name":"alsa_output.pci-0000_00_1b.0.analog-stereo",
          "node.description":"Built-in Audio Analog Stereo",
          "device.id":51,
          "card.profile.device":3}}},
      {"id":63,"type":"PipeWire:Interface:Device","info":{
        "props":{
          "media.class":"Audio/Device",
          "device.name":"alsa_card.usb-Focusrite_Scarlett_2i2_USB-00",
          "device.description":"Focusrite Scarlett 2i2",
          "device.bus":"usb",
          "device.api":"alsa"},
        "params":{"Route":[
          {"index":1,"direction":"Output","name":"analog-output",
           "available":"unknown","device":3}]}}},
      {"id":65,"type":"PipeWire:Interface:Node","info":{
        "props":{
          "media.class":"Audio/Sink",
          "node.name":"alsa_output.usb-Focusrite_Scarlett_2i2_USB-00.analog-stereo",
          "node.description":"Focusrite Scarlett 2i2 Analog Stereo",
          "device.id":63,
          "card.profile.device":3}}},
      {"id":99,"type":"PipeWire:Interface:Node","info":{
        "props":{"media.class":"Stream/Output/Audio","node.name":"some-app"}}},
      {"id":7,"type":"PipeWire:Interface:Metadata","metadata":[
        {"key":"default.audio.sink",
         "value":{"name":"alsa_output.usb-Focusrite_Scarlett_2i2_USB-00.analog-stereo"}}]}
    ]"#;

    #[test]
    fn enumerates_both_outputs_and_resolves_the_default() {
        let topo = parse_pw_dump(TWO_DEVICE_DUMP);
        // The Stream/Output/Audio node is not an output.
        assert_eq!(topo.outputs.len(), 2);

        let current = topo.current_output().expect("default sink resolves");
        assert_eq!(
            current.device_name,
            "alsa_card.usb-Focusrite_Scarlett_2i2_USB-00"
        );
        assert_eq!(current.route_name.as_deref(), Some("analog-output"));
        assert_eq!(current.description, "Focusrite Scarlett 2i2 Analog Stereo");
    }

    #[test]
    fn key_matches_wireplumber_default_routes_format() {
        let topo = parse_pw_dump(TWO_DEVICE_DUMP);
        let builtin = topo
            .outputs
            .iter()
            .find(|o| o.device_name.starts_with("alsa_card.pci"))
            .unwrap();
        // Byte-for-byte the key WirePlumber writes into its `default-routes`
        // state file for this card and port.
        assert_eq!(
            builtin.key(),
            "alsa_card.pci-0000_00_1b.0:output:analog-output-lineout"
        );
    }

    #[test]
    fn a_key_resolves_to_the_node_id_from_the_same_dump() {
        let topo = parse_pw_dump(TWO_DEVICE_DUMP);
        let builtin = topo
            .output_by_key("alsa_card.pci-0000_00_1b.0:output:analog-output-lineout")
            .expect("the built-in card is in this dump");
        // The one place an object id is legitimate: resolved from the dump it is
        // used with, for a single `wpctl set-default`, and never kept.
        assert_eq!(topo.node_id_of(builtin), Some(52));

        // Selecting is keyed on identity, so a device that is not in this dump
        // simply is not there — which is what a remembered row for unplugged
        // headphones looks like.
        assert!(
            topo.output_by_key("alsa_card.absent:output:analog-output")
                .is_none()
        );
    }

    #[test]
    fn unknown_jack_availability_still_counts_as_usable() {
        // Neither card does jack detection; both report "unknown". Treating that
        // as unavailable would hide every output on such a host.
        let topo = parse_pw_dump(TWO_DEVICE_DUMP);
        assert!(topo.outputs.iter().all(|o| o.usable));
    }

    #[test]
    fn explicit_unavailable_route_is_not_usable() {
        let json = br#"[
          {"id":1,"type":"PipeWire:Interface:Device","info":{
            "props":{"media.class":"Audio/Device","device.name":"alsa_card.x"},
            "params":{"Route":[{"direction":"Output","name":"analog-output-headphones",
                       "available":"no","device":0}]}}},
          {"id":2,"type":"PipeWire:Interface:Node","info":{
            "props":{"media.class":"Audio/Sink","node.name":"alsa_output.x",
                     "device.id":1,"card.profile.device":0}}}
        ]"#;
        let topo = parse_pw_dump(json);
        assert!(!topo.outputs[0].usable);
    }

    #[test]
    fn classification_is_honest_about_what_it_cannot_tell() {
        let topo = parse_pw_dump(TWO_DEVICE_DUMP);
        // A generic USB interface: route is the uninformative `analog-output` and
        // udev sets no form-factor, so nothing can classify it.
        let usb = topo.current_output().unwrap();
        assert_eq!(usb.kind, AudioOutputKind::Unknown);
        // The built-in reports form-factor `internal`, which describes the card
        // and not the live port, so it must not be read as a classification. The
        // route says line-out, and that is all we actually know.
        let builtin = topo
            .outputs
            .iter()
            .find(|o| o.device_name.starts_with("alsa_card.pci"))
            .unwrap();
        assert_eq!(builtin.kind, AudioOutputKind::LineOut);
    }

    #[test]
    fn route_kind_covers_the_shipped_port_vocabulary() {
        // Stems taken from /usr/share/alsa-card-profile/mixer/paths/.
        for (route, want) in [
            (
                "analog-output-headphones",
                Some(AudioOutputKind::Headphones),
            ),
            (
                "analog-output-headphones-2",
                Some(AudioOutputKind::Headphones),
            ),
            ("analog-output-chat", Some(AudioOutputKind::Headphones)),
            (
                "usb-gaming-headset-output-stereo",
                Some(AudioOutputKind::Headphones),
            ),
            ("analog-output-speaker", Some(AudioOutputKind::Speakers)),
            (
                "analog-output-speaker-always",
                Some(AudioOutputKind::Speakers),
            ),
            ("hdmi-output-0", Some(AudioOutputKind::Hdmi)),
            ("hdmi-output-10", Some(AudioOutputKind::Hdmi)),
            ("iec958-stereo-output", Some(AudioOutputKind::Digital)),
            ("analog-output-lineout", Some(AudioOutputKind::LineOut)),
            // Generic ports carry no port-level signal, and must say so rather
            // than guessing.
            ("analog-output", None),
            ("analog-output-mono", None),
            ("audigy-analog-output", None),
        ] {
            assert_eq!(route_kind(route), want, "route {route}");
        }
    }

    #[test]
    fn bluetooth_is_classified_by_transport_without_a_route() {
        let json = br#"[
          {"id":1,"type":"PipeWire:Interface:Device","info":{
            "props":{"media.class":"Audio/Device","device.api":"bluez5",
                     "device.name":"bluez_card.AA_BB_CC_DD_EE_FF",
                     "device.form-factor":"headset"}}},
          {"id":2,"type":"PipeWire:Interface:Node","info":{
            "props":{"media.class":"Audio/Sink","device.id":1,
                     "node.name":"bluez_output.AA_BB_CC_DD_EE_FF.1",
                     "node.description":"Some Headset"}}}
        ]"#;
        let topo = parse_pw_dump(json);
        let out = &topo.outputs[0];
        assert_eq!(out.kind, AudioOutputKind::Bluetooth);
        // No ALSA route concept, so the key degrades to the device alone.
        assert_eq!(out.route_name, None);
        assert_eq!(out.key(), "bluez_card.AA_BB_CC_DD_EE_FF:output");
        assert!(out.usable);
    }

    #[test]
    fn hdmi_sinks_are_still_flagged_for_the_dock_router() {
        let json = br#"[
          {"id":40,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Audio/Sink",
            "node.name":"alsa_output.pci-0000_00_1f.3.analog-stereo",
            "node.description":"Built-in Audio"}}},
          {"id":51,"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Audio/Sink",
            "node.name":"alsa_output.pci-0000_01_00.1.hdmi-stereo",
            "api.alsa.path":"hdmi:0",
            "node.description":"GPU HDMI Audio"}}}
        ]"#;
        let topo = parse_pw_dump(json);
        assert_eq!(topo.sinks.len(), 2);
        assert!(
            topo.sinks
                .iter()
                .find(|s| s.id == 51)
                .unwrap()
                .is_external_video
        );
        assert!(
            !topo
                .sinks
                .iter()
                .find(|s| s.id == 40)
                .unwrap()
                .is_external_video
        );
    }

    #[test]
    fn garbage_input_yields_an_empty_topology() {
        let topo = parse_pw_dump(b"not json");
        assert!(topo.outputs.is_empty());
        assert!(topo.sinks.is_empty());
        assert!(topo.default_sink.is_none());
    }
}

/// Checks against the machine's real PipeWire rather than a fixture. Ignored by
/// default because it needs a running PipeWire; run with
/// `cargo test -p shepherd-host-linux -- --ignored --nocapture live_pw_dump`.
#[cfg(test)]
mod live_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a running PipeWire"]
    async fn live_pw_dump_enumerates_outputs() {
        let topo = match dump().await {
            Ok(topo) => topo,
            Err(e) => panic!("could not read the audio topology: {e}"),
        };
        assert!(!topo.outputs.is_empty(), "no outputs found");
        for o in &topo.outputs {
            let marker = if Some(o.node_name.as_str()) == topo.default_sink.as_deref() {
                "*"
            } else {
                " "
            };
            println!(
                "{marker} {:<44} kind={:<10} usable={} {}",
                o.key(),
                o.kind.as_str(),
                o.usable,
                o.description
            );
        }
        assert!(
            topo.current_output().is_some(),
            "default sink did not resolve to an enumerated output"
        );
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn cubic_volume_matches_what_wpctl_reports() {
        // PipeWire stores the cube; `wpctl get-volume` prints the cube root.
        // These pairs were read off a live host to pin the convention.
        let cases = [
            (vec![0.015_625_f64], 25u8), // cbrt = 0.25
            (vec![0.063_997], 40),       // cbrt ~= 0.4
            (vec![1.0], 100),
            (vec![0.0], 0),
        ];
        for (channels, want) in cases {
            let props = RawProps {
                channel_volumes: channels.clone(),
                volume: None,
                mute: false,
            };
            assert_eq!(cubic_to_percent(&props), want, "channels {channels:?}");
        }
    }

    #[test]
    fn loudest_channel_wins_and_volume_is_the_fallback() {
        let props = RawProps {
            channel_volumes: vec![0.015_625, 0.063_997],
            volume: None,
            mute: false,
        };
        assert_eq!(cubic_to_percent(&props), 40);

        let props = RawProps {
            channel_volumes: vec![],
            volume: Some(0.015_625),
            mute: false,
        };
        assert_eq!(cubic_to_percent(&props), 25);
    }

    #[test]
    fn volume_and_identity_come_from_one_read() {
        // The whole point of sourcing both from a single dump: the pair cannot
        // describe two different moments.
        let json = br#"[
          {"id":1,"type":"PipeWire:Interface:Device","info":{
            "props":{"media.class":"Audio/Device","device.name":"alsa_card.x"},
            "params":{"Route":[{"direction":"Output","name":"analog-output-headphones",
                       "available":"unknown","device":0}]}}},
          {"id":2,"type":"PipeWire:Interface:Node","info":{
            "props":{"media.class":"Audio/Sink","node.name":"alsa_output.x",
                     "node.description":"Cans","device.id":1,"card.profile.device":0},
            "params":{"Props":[{"channelVolumes":[0.063997],"mute":true}]}}},
          {"id":7,"type":"PipeWire:Interface:Metadata","metadata":[
            {"key":"default.audio.sink","value":{"name":"alsa_output.x"}}]}
        ]"#;
        let topo = parse_pw_dump(json);
        let cur = topo.current_output().unwrap();
        assert_eq!(cur.volume_percent, 40);
        assert!(cur.muted);
        assert_eq!(cur.kind, AudioOutputKind::Headphones);
        assert_eq!(cur.key(), "alsa_card.x:output:analog-output-headphones");
    }
}
