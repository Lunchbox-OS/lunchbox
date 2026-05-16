//! shepherd-gamepad-bridge: translate gamepad input into Wayland mouse +
//! keyboard events.
//!
//! Run alongside an activity that doesn't process raw gamepad input, or
//! for which the gamepad doesn't match the activity's interaction model.
//! The bridge polls gilrs for gamepad events, translates them per the
//! configured preset on a 125 Hz tick, and emits the results through the
//! wlroots virtual-pointer protocol and the unstable virtual-keyboard
//! protocol.

mod gamepad;
mod preset;
mod wl;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use gilrs::EventType;
use tracing::info;

use crate::preset::{Preset, PresetState, Tunables};
use crate::wl::WaylandOutputs;

#[derive(Parser, Debug)]
#[command(
    name = "shepherd-gamepad-bridge",
    about = "Translate gamepad input to Wayland virtual-pointer + virtual-keyboard events"
)]
struct Args {
    /// Mapping preset.
    #[arg(long, value_enum)]
    preset: PresetArg,

    /// Stick deadzone (fraction of full deflection, 0..1).
    #[arg(long)]
    deadzone: Option<f32>,

    /// Mouse speed in px/sec at full stick deflection.
    #[arg(long)]
    mouse_speed: Option<f32>,

    /// Scroll speed in wheel notches/sec at full deflection.
    #[arg(long)]
    scroll_speed: Option<f32>,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum PresetArg {
    Productivity,
    Gpd,
}

impl From<PresetArg> for Preset {
    fn from(p: PresetArg) -> Self {
        match p {
            PresetArg::Productivity => Preset::Productivity,
            PresetArg::Gpd => Preset::Gpd,
        }
    }
}

fn install_signal_handlers(shutdown: Arc<AtomicBool>) -> Result<()> {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

    static SHUTDOWN_FLAG: AtomicBool = AtomicBool::new(false);
    extern "C" fn handler(_: i32) {
        SHUTDOWN_FLAG.store(true, Ordering::SeqCst);
    }

    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    // SAFETY: Installing a signal handler that only sets an atomic flag is
    // async-signal-safe.
    unsafe {
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGINT, &action)?;
    }

    thread::Builder::new()
        .name("gamepad-bridge-signals".into())
        .spawn(move || {
            loop {
                if SHUTDOWN_FLAG.load(Ordering::SeqCst) {
                    shutdown.store(true, Ordering::SeqCst);
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        })?;
    Ok(())
}

fn millis_since(start: Instant) -> u32 {
    start.elapsed().as_millis() as u32
}

fn build_tunables(args: &Args) -> Tunables {
    let defaults = Tunables::default();
    Tunables {
        deadzone: args.deadzone.unwrap_or(defaults.deadzone),
        mouse_speed: args.mouse_speed.unwrap_or(defaults.mouse_speed),
        scroll_speed: args.scroll_speed.unwrap_or(defaults.scroll_speed),
        ..defaults
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let preset: Preset = args.preset.into();
    let tunables = build_tunables(&args);
    info!(
        preset = ?preset,
        deadzone = tunables.deadzone,
        mouse_speed = tunables.mouse_speed,
        scroll_speed = tunables.scroll_speed,
        "Starting gamepad bridge"
    );

    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(shutdown.clone())?;

    let mut gilrs = gamepad::init()?;
    gamepad::log_connected(&gilrs);

    let mut outputs = WaylandOutputs::connect()?;
    info!("Gamepad bridge ready");

    let mut state = PresetState::new(preset, tunables);
    let start = Instant::now();
    let mut last_tick = Instant::now();
    let tick_interval = Duration::from_millis(8); // 125 Hz

    while !shutdown.load(Ordering::SeqCst) {
        // Pump every pending gilrs event into preset state. gilrs handles
        // hotplug for us: gamepads connected after startup show up
        // automatically on subsequent polls.
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::ButtonPressed(button, _) => state.ingest_button(button, true),
                EventType::ButtonReleased(button, _) => state.ingest_button(button, false),
                EventType::AxisChanged(axis, value, _) => state.ingest_axis(axis, value),
                EventType::Connected => {
                    let name = gilrs
                        .connected_gamepad(event.id)
                        .map(|g| g.name().to_string())
                        .unwrap_or_default();
                    info!(id = ?event.id, name = %name, "Gamepad connected");
                }
                EventType::Disconnected => {
                    info!(id = ?event.id, "Gamepad disconnected");
                }
                _ => {}
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last_tick);
        if dt >= tick_interval {
            last_tick = now;
            let events = state.tick(dt);
            if !events.is_empty() {
                let t = millis_since(start);
                for ev in events {
                    outputs.dispatch(ev, t);
                }
                outputs.frame();
            }
            outputs.flush()?;
        } else {
            // Sleep until the next tick boundary, capped so we stay
            // responsive to incoming events.
            let until_next = tick_interval - dt;
            thread::sleep(until_next.min(Duration::from_millis(4)));
        }
    }

    // Send releases for everything still latched as held so we don't leave
    // a phantom key or button down in the compositor.
    info!("Shutting down gamepad bridge");
    let releases = state.drain_held();
    if !releases.is_empty() {
        let t = millis_since(start);
        for ev in releases {
            outputs.dispatch(ev, t);
        }
        outputs.frame();
    }
    let _ = outputs.flush();
    outputs.destroy();
    Ok(())
}
