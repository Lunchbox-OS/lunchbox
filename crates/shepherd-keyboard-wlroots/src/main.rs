//! wlroots backend for the swipe keyboard.
//!
//! A standalone `smithay-client-toolkit` Wayland client that renders the keyboard on a
//! bottom-anchored layer-shell surface, captures touch (with a pointer fallback), decodes
//! swipes via `shepherd-keyboard-core`, and commits text through `input-method-v2`
//! (`virtual-keyboard` for non-text keys). It depends only on standard Wayland protocols, so
//! it runs on any wlroots compositor and stays extractable.

mod render;
mod vkbd;

use std::cell::{Cell, RefCell};
use std::num::NonZeroU32;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_input_method, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, delegate_touch,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        input_method::{
            Active, CursorPosition, InputMethod, InputMethodEventState, InputMethodHandler,
            InputMethodManager,
        },
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        touch::TouchHandler,
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface, wl_touch},
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

use shepherd_keyboard_core::{
    ContentType, GestureBuilder, HostAction, InputPurpose, Keyboard, Profile, RawPoint, bundle,
    fallback_layout,
};

use crate::render::{Hit, Regions};
use crate::vkbd::VirtualKeyboard;

/// CLI for the wlroots swipe-keyboard backend.
#[derive(Parser, Debug)]
#[command(name = "shepherd-keyboard-wlroots")]
struct Args {
    /// Safety profile to load (adult | child).
    #[arg(long, default_value = "adult")]
    profile: String,
    /// Bundle root directory (contains `adult/` and `child/`).
    #[arg(long, env = "SHEPHERD_SWIPE_BUNDLE_DIR")]
    bundle_dir: Option<PathBuf>,
    /// Trusted minisign public key file (production trust anchor). Uses the decoder's
    /// committed dev key when unset.
    #[arg(long)]
    public_key: Option<PathBuf>,
    /// Surface height in pixels.
    #[arg(long, default_value_t = 320)]
    height: u32,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    let kbd = build_keyboard(&args)?;

    let conn = Connection::connect_to_env().context("connect to Wayland display")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("init registry")?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("wlr layer shell missing")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm missing")?;
    let input_method_manager = InputMethodManager::bind(&globals, &qh).ok();
    if input_method_manager.is_none() {
        tracing::warn!("zwp_input_method_manager_v2 not advertised; text commit unavailable");
    }
    let vkbd_manager = globals
        .bind::<ZwpVirtualKeyboardManagerV1, _, _>(&qh, 1..=1, ())
        .ok();

    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some("shepherd-keyboard"), None);
    layer.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(0, args.height);
    layer.set_exclusive_zone(args.height as i32);
    layer.commit();

    let pool = SlotPool::new(1920 * args.height as usize * 4, &shm).context("create shm pool")?;

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        width: 0,
        height: args.height,
        first_configure: true,
        exit: Cell::new(false),
        font: render::Font::load(),
        kbd,
        input_method_manager,
        input_method: None,
        im_active: false,
        vkbd_manager,
        vkbd: None,
        touch: None,
        pointer: None,
        seat_initialized: false,
        active: None,
        tick: 0,
        pending_im: RefCell::new(None),
    };

    loop {
        event_queue.blocking_dispatch(&mut app)?;
        app.process_pending_im(&qh);
        let _ = conn.flush();
        if app.exit.get() {
            break;
        }
    }
    Ok(())
}

/// Load the profile's signed bundle, failing closed to tap-only on any error.
fn build_keyboard(args: &Args) -> anyhow::Result<Keyboard> {
    let profile = Profile::parse(&args.profile).unwrap_or(Profile::Adult);
    let root = args
        .bundle_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("dev-runtime/swipe-bundles"));
    let dir = bundle::bundle_dir(&root, profile);
    let public_key = match &args.public_key {
        Some(path) => Some(std::fs::read_to_string(path).context("read public key file")?),
        None => None,
    };
    match bundle::load_decoder(&dir, public_key.as_deref()) {
        Ok(decoder) => {
            tracing::info!(%profile, dir = %dir.display(), "loaded signed bundle");
            Ok(Keyboard::new(decoder))
        }
        Err(e) => {
            tracing::warn!(%profile, dir = %dir.display(), error = %e,
                "failed to load bundle; failing closed to tap-only (no predictions)");
            Ok(Keyboard::tap_only(fallback_layout()))
        }
    }
}

/// One in-progress touch/pointer interaction (single-touch only).
struct Active2 {
    id: i32,
    hit: Hit,
    builder: Option<GestureBuilder>,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    width: u32,
    height: u32,
    first_configure: bool,
    exit: Cell<bool>,
    font: render::Font,

    kbd: Keyboard,
    input_method_manager: Option<InputMethodManager>,
    input_method: Option<InputMethod>,
    im_active: bool,
    vkbd_manager: Option<ZwpVirtualKeyboardManagerV1>,
    vkbd: Option<VirtualKeyboard>,
    touch: Option<wl_touch::WlTouch>,
    pointer: Option<wl_pointer::WlPointer>,
    seat_initialized: bool,
    active: Option<Active2>,
    tick: u32,
    // Filled by the (&self) input-method handler; drained by the main loop with &mut self.
    pending_im: RefCell<Option<InputMethodEventState>>,
}

impl App {
    fn regions(&self) -> Regions {
        Regions::new(self.width.max(1), self.height.max(1))
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        let (width, height) = (self.width, self.height);
        let stride = width as i32 * 4;
        let regions = self.regions();
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .expect("create shm buffer");
        render::draw(
            canvas,
            width,
            height,
            &regions,
            self.kbd.layout(),
            &self.font,
            self.kbd.suggestions(),
            self.kbd.swipe_enabled(),
        );
        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, width as i32, height as i32);
        buffer.attach_to(surface).expect("attach buffer");
        self.layer.commit();
    }

    /// Perform the host actions the core returned.
    fn apply(&mut self, actions: Vec<HostAction>, qh: &QueueHandle<Self>) {
        let mut redraw = false;
        for action in actions {
            match action {
                HostAction::CommitText(text) => {
                    if let Some(im) = &self.input_method {
                        im.commit_string(text);
                        im.commit();
                    }
                }
                HostAction::SetPreedit(text) => {
                    if let Some(im) = &self.input_method {
                        let cursor = if text.is_empty() {
                            CursorPosition::Hidden
                        } else {
                            let n = text.len();
                            CursorPosition::Visible { start: n, end: n }
                        };
                        im.set_preedit_string(text, cursor);
                        im.commit();
                    }
                }
                HostAction::KeyInput(sym) => {
                    if let Some(vk) = &mut self.vkbd {
                        vk.key(sym);
                    }
                }
                HostAction::SetSuggestions(_) => redraw = true,
            }
        }
        if redraw {
            self.draw(qh);
        }
    }

    fn begin_interaction(&mut self, id: i32, x: f32, y: f32, t_ms: u32) {
        if self.active.is_some() {
            return; // single-touch only
        }
        let regions = self.regions();
        let hit = regions.hit(x, y);
        let builder = if hit == Hit::KeyArea {
            let mut b = GestureBuilder::new(regions.key_area());
            b.push(RawPoint { x, y, t_ms });
            Some(b)
        } else {
            None
        };
        self.active = Some(Active2 { id, hit, builder });
    }

    fn move_interaction(&mut self, id: i32, x: f32, y: f32, t_ms: u32) {
        if let Some(active) = &mut self.active
            && active.id == id
            && let Some(builder) = &mut active.builder
        {
            builder.push(RawPoint { x, y, t_ms });
        }
    }

    fn end_interaction(&mut self, id: i32, qh: &QueueHandle<Self>) {
        let Some(active) = self.active.take() else {
            return;
        };
        if active.id != id {
            self.active = Some(active);
            return;
        }
        if !self.im_active {
            return; // no focused text field
        }
        let actions = match active.hit {
            Hit::Suggestion(slot) => self.kbd.select_suggestion(slot),
            Hit::Function(key) => self.kbd.on_function_key(key),
            Hit::KeyArea => match active.builder.and_then(|b| b.finish()) {
                Some(stroke) => self.kbd.on_stroke(stroke),
                None => Vec::new(),
            },
        };
        self.apply(actions, qh);
        self.draw(qh);
    }

    /// Drain a pending input-method state change (focus / content-type / surrounding text).
    fn process_pending_im(&mut self, qh: &QueueHandle<Self>) {
        let Some(state) = self.pending_im.borrow_mut().take() else {
            return;
        };
        let active = matches!(state.active, Active::Active { .. });
        let content_type = ContentType {
            purpose: map_purpose(state.content_purpose),
            sensitive_hint: state.content_hint.contains(ContentHint::SensitiveData),
        };
        let actions = self.kbd.set_content_type(content_type);
        // Preceding text = surrounding text up to the cursor (gate ignores it when sensitive).
        let cursor = (state.surrounding.cursor as usize).min(state.surrounding.text.len());
        self.kbd
            .set_surrounding_text(&state.surrounding.text[..cursor]);
        self.apply(actions, qh);
        self.im_active = active;
        self.draw(qh);
    }

    fn ensure_seat_objects(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) {
        if self.seat_initialized {
            return;
        }
        self.seat_initialized = true;
        if let Some(manager) = &self.input_method_manager {
            self.input_method = Some(manager.get_input_method(qh, seat));
        }
        if let Some(manager) = &self.vkbd_manager {
            match VirtualKeyboard::new(manager, seat, qh) {
                Ok(vk) => self.vkbd = Some(vk),
                Err(e) => tracing::warn!(error = %e, "virtual keyboard unavailable"),
            }
        }
    }
}

fn map_purpose(purpose: ContentPurpose) -> InputPurpose {
    match purpose {
        ContentPurpose::Normal => InputPurpose::Normal,
        ContentPurpose::Alpha => InputPurpose::Alpha,
        ContentPurpose::Digits => InputPurpose::Digits,
        ContentPurpose::Number => InputPurpose::Number,
        ContentPurpose::Phone => InputPurpose::Phone,
        ContentPurpose::Url => InputPurpose::Url,
        ContentPurpose::Email => InputPurpose::Email,
        ContentPurpose::Name => InputPurpose::Name,
        ContentPurpose::Password => InputPurpose::Password,
        ContentPurpose::Pin => InputPurpose::Pin,
        ContentPurpose::Date => InputPurpose::Date,
        ContentPurpose::Time => InputPurpose::Time,
        ContentPurpose::Datetime => InputPurpose::Datetime,
        ContentPurpose::Terminal => InputPurpose::Terminal,
        _ => InputPurpose::Normal,
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw(qh);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit.set(true);
    }
    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.width = NonZeroU32::new(configure.new_size.0).map_or(self.width.max(640), |v| v.get());
        self.height = NonZeroU32::new(configure.new_size.1).map_or(self.height, |v| v.get());
        if self.first_configure {
            self.first_configure = false;
        }
        self.draw(qh);
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.ensure_seat_objects(qh, &seat);
    }
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        self.ensure_seat_objects(qh, &seat);
        if capability == Capability::Touch
            && self.touch.is_none()
            && let Ok(touch) = self.seat_state.get_touch(qh, &seat)
        {
            self.touch = Some(touch);
        }
        if capability == Capability::Pointer
            && self.pointer.is_none()
            && let Ok(pointer) = self.seat_state.get_pointer(qh, &seat)
        {
            self.pointer = Some(pointer);
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Touch
            && let Some(t) = self.touch.take()
        {
            t.release();
        }
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
        {
            p.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl TouchHandler for App {
    #[allow(clippy::too_many_arguments)]
    fn down(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        _serial: u32,
        time: u32,
        surface: wl_surface::WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        if &surface != self.layer.wl_surface() {
            return;
        }
        self.begin_interaction(id, position.0 as f32, position.1 as f32, time);
    }
    fn up(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        _serial: u32,
        _time: u32,
        id: i32,
    ) {
        self.end_interaction(id, qh);
    }
    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        self.move_interaction(id, position.0 as f32, position.1 as f32, time);
    }
    fn shape(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        _: i32,
        _: f64,
        _: f64,
    ) {
    }
    fn orientation(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_touch::WlTouch,
        _: i32,
        _: f64,
    ) {
    }
    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_touch::WlTouch) {
        self.active = None;
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        const POINTER_ID: i32 = -1;
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            let (x, y) = (event.position.0 as f32, event.position.1 as f32);
            self.tick = self.tick.wrapping_add(8);
            match event.kind {
                PointerEventKind::Press { .. } => {
                    self.begin_interaction(POINTER_ID, x, y, self.tick)
                }
                PointerEventKind::Motion { .. } => {
                    self.move_interaction(POINTER_ID, x, y, self.tick)
                }
                PointerEventKind::Release { .. } => self.end_interaction(POINTER_ID, qh),
                _ => {}
            }
        }
    }
}

impl InputMethodHandler for App {
    fn handle_done(
        &self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::seat::input_method::ZwpInputMethodV2,
        state: &InputMethodEventState,
    ) {
        *self.pending_im.borrow_mut() = Some(state.clone());
    }
    fn handle_unavailable(
        &self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &smithay_client_toolkit::seat::input_method::ZwpInputMethodV2,
    ) {
        tracing::error!("input method became unavailable; another IME is likely active");
        self.exit.set(true);
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

// The virtual-keyboard interfaces have no events; their dispatch is a no-op.
impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<ZwpVirtualKeyboardV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_touch!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_input_method!(App);
delegate_registry!(App);
