//! HUD Application
//!
//! The main GTK4 application for the HUD overlay.
//! Uses gtk4-layer-shell to create an always-visible overlay.

use crate::battery::BatteryStatus;
use crate::orientation::HudOrientationExt;
use crate::rotated_label::RotatedLabel;
use crate::state::{SessionState, SharedState};
use crate::time_display::TimeDisplay;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use lunchbox_api::HudOrientation;
use lunchbox_ipc::IpcClient;
use lunchbox_util::default_socket_path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
use tokio::runtime::Runtime;

/// Send a one-shot RPC to lunchboxd on a background thread. The HUD's
/// GTK main loop must never block on IPC, so each action button spins
/// up a short-lived Tokio runtime, connects, calls, exits. Errors are
/// logged (there is no UI surface to report them to). `action` runs
/// against a freshly-connected client and returns any error the caller
/// wants to see in the log.
fn spawn_action<F, Fut>(socket_path: PathBuf, label: &'static str, action: F)
where
    F: FnOnce(IpcClient) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = lunchbox_ipc::IpcResult<()>> + Send,
{
    std::thread::spawn(move || {
        let rt = Runtime::new().expect("Failed to create runtime");
        rt.block_on(async move {
            match IpcClient::connect(&socket_path).await {
                Ok(client) => {
                    if let Err(e) = action(client).await {
                        tracing::error!("Failed to send {}: {}", label, e);
                    }
                }
                Err(e) => tracing::error!("Failed to connect to lunchboxd: {}", e),
            }
        });
    });
}

/// How many characters of a window's title the taskbar shows.
///
/// Long titles are the norm — browsers put the whole page title in them — and
/// this row shares the bar with the clock, two sliders and four buttons.
const TASKBAR_LABEL_CHARS: usize = 24;

/// What a window is called in the taskbar.
///
/// Same fallback chain the two management clients use (`name` → `app_id` →
/// `class` → something generic), so a window is called the same thing wherever
/// a caregiver sees it.
fn taskbar_label(w: &lunchbox_api::WindowInfo) -> String {
    let title = w
        .name
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .or(w.app_id.as_deref())
        .or(w.window_class.as_deref())
        .unwrap_or("Window");
    let mut label: String = title.chars().take(TASKBAR_LABEL_CHARS).collect();
    if title.chars().count() > TASKBAR_LABEL_CHARS {
        label.push('…');
    }
    label
}

/// Redraw the taskbar's window buttons.
///
/// Rebuilt wholesale rather than diffed: there are a handful of buttons, they
/// change only when a window opens or closes, and the alternative is keeping a
/// parallel model of the row in sync with the compositor's — which is the kind
/// of bookkeeping that goes wrong quietly. Skipped entirely when the labels
/// already match, so the common tick costs one comparison and the focused
/// window's button does not lose a press mid-click.
fn rebuild_taskbar(row: &gtk4::Box, windows: &[lunchbox_api::WindowInfo]) {
    let wanted: Vec<(u64, String, bool)> = windows
        .iter()
        .map(|w| (w.id, taskbar_label(w), w.focused))
        .collect();

    let mut existing = Vec::new();
    let mut child = row.first_child();
    while let Some(w) = child {
        child = w.next_sibling();
        existing.push(w);
    }
    let unchanged = existing.len() == wanted.len()
        && existing
            .iter()
            .zip(&wanted)
            .all(|(widget, (_, label, focused))| {
                widget.downcast_ref::<gtk4::Button>().is_some_and(|b| {
                    b.label().is_some_and(|l| l == label.as_str())
                        && b.has_css_class("taskbar-focused") == *focused
                })
            });
    if unchanged {
        return;
    }

    for widget in existing {
        row.remove(&widget);
    }
    for (id, label, focused) in wanted {
        let button = gtk4::Button::builder()
            .label(&label)
            .tooltip_text(&label)
            .has_frame(false)
            .build();
        button.add_css_class("indicator-button");
        if focused {
            button.add_css_class("taskbar-focused");
        }
        button.connect_clicked(move |_| {
            spawn_action(
                default_socket_path(),
                "focus window",
                move |mut client| async move {
                    client
                        .act_on_window(id, lunchbox_api::WindowAction::Focus)
                        .await
                },
            );
        });
        row.append(&button);
    }
}

/// The launcher's own window, which administrator mode's "Apps" button raises.
fn shell_window_id(windows: &[lunchbox_api::WindowInfo]) -> Option<u64> {
    windows
        .iter()
        .find(|w| w.app_id.as_deref() == Some("com.lunchboxos.launcher"))
        .map(|w| w.id)
}

/// The windows a caregiver opened: everything that is not Lunchbox's own
/// furniture, **including the ones stashed on the scratchpad**. What the
/// taskbar lists.
///
/// The scratchpad is this compositor's "minimized", and a taskbar is how a
/// minimized window comes back — so a stashed window needs a button, and
/// pressing it works: `WindowAction::Focus` pulls a window off the scratchpad
/// as well as raising it. It matters more here than it looks. `sway.conf`'s
/// `for_window [class="^[Ss]team$"] move scratchpad` rules are not part of the
/// admin binding mode and cannot be — `for_window` is evaluated at map time —
/// so Steam launched from the picker is stashed the moment it maps. Without a
/// button for it, "log into Steam", the first thing issue #154 asks for, cannot
/// be done from the device at all.
fn admin_windows(windows: &[lunchbox_api::WindowInfo]) -> Vec<lunchbox_api::WindowInfo> {
    windows
        .iter()
        .filter(|w| w.owner != lunchbox_api::WindowOwner::Lunchbox)
        .cloned()
        .collect()
}

/// The subset of [`admin_windows`] that is actually on screen. What the "X"
/// closes one of, and what decides whether it offers the exit instead.
///
/// A separate question from what the taskbar lists, and the two must not share
/// an answer. The preloaded Steam client sits stashed for the life of the
/// session and the compositor reports it as belonging to nothing Lunchbox
/// knows about — `snap run` re-execs, so the pid the host recorded is not the
/// pid that draws — so counting stashed windows here would mean the "X" never
/// offered the way out on any device that preloads Steam. Which is every device
/// that has it configured.
fn admin_windows_on_screen(windows: &[lunchbox_api::WindowInfo]) -> Vec<lunchbox_api::WindowInfo> {
    admin_windows(windows)
        .into_iter()
        .filter(|w| !w.in_scratchpad)
        .collect()
}

/// Ask lunchboxd to end the current session gracefully (the "X" button).
fn request_stop_current(socket_path: PathBuf) {
    tracing::info!("Requesting end session");
    spawn_action(socket_path, "stop_current", |mut client| async move {
        client.stop_current(lunchbox_api::StopMode::Graceful).await
    });
}

/// Ask lunchboxd to reset the current activity — the "reboot the console"
/// button (issue #125). The session keeps running; only the activity restarts.
fn request_reset_current(socket_path: PathBuf) {
    tracing::info!("Requesting activity reset");
    spawn_action(socket_path, "reset_current", |mut client| async move {
        client.reset_current().await
    });
}

/// What a confirmation prompt does when its affirmative button is pressed.
///
/// Both prompts lose something the child cares about, so both confirm and both
/// use the destructive styling; they differ only in wording and in which RPC
/// they send.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmAction {
    /// End the session (issue #78).
    EndActivity,
    /// Restart the activity at its starting state (issue #125).
    ResetActivity,
}

impl ConfirmAction {
    /// Label for the affirmative button.
    fn button_label(self) -> &'static str {
        match self {
            Self::EndActivity => "End activity",
            Self::ResetActivity => "Restart",
        }
    }

    /// The question, naming the activity when the HUD knows it.
    fn message(self, activity: Option<&str>) -> String {
        match (self, activity) {
            (Self::EndActivity, Some(name)) => {
                format!("End {name}? Unsaved progress may be lost.")
            }
            (Self::EndActivity, None) => "End this activity? Unsaved progress may be lost.".into(),
            (Self::ResetActivity, Some(name)) => {
                format!("Restart {name} from the beginning? Your saved game is kept.")
            }
            (Self::ResetActivity, None) => {
                "Restart this activity from the beginning? Your saved game is kept.".into()
            }
        }
    }

    fn run(self, socket_path: PathBuf) {
        match self {
            Self::EndActivity => request_stop_current(socket_path),
            Self::ResetActivity => request_reset_current(socket_path),
        }
    }
}

/// Pixel size for all symbolic icons in the HUD bar at scale 1.0. The
/// timer in `build_hud_content` multiplies this by the current HUD scale
/// factor so icons stay at their usual physical size when lunchboxd drops
/// the compositor scale for an XWayland activity.
const BASE_ICON_PIXEL_SIZE: i32 = 20;

/// Logical-pixel length of a pop-out slider at scale 1.0, scaled the same way
/// as the icon size above so the slider grows with the rest of the HUD.
///
/// Longer than the 80px the sliders had while they sat in the bar. Once a
/// control opens into a flyout, its length is nobody's cost but its own — the
/// bar pays for a 32px icon either way — so the slider gets the room that
/// makes it easy to hit precisely on a touchscreen (issue #178).
const BASE_SLIDER_LENGTH: i32 = 140;

/// Whether the bar should be carrying the page-turn buttons.
///
/// Debug builds additionally honour `LUNCHBOX_HUD_DEBUG_FORCE_PAGE_BUTTONS`,
/// because the reading session is the bar's worst case for room and there is
/// otherwise no way to *look* at it: the headless dev session has no reader to
/// start (okular is not installed there), so every previous attempt at this —
/// issue #171's layout review, and issue #178's — had to add a throwaway
/// override, screenshot, and take it out again. Making it permanent is what
/// lets the layout be checked at any screen size on demand. Never compiled
/// into a release build, and deliberately *not* consulted by the button
/// handlers: forcing the buttons visible must not let a stray press send page
/// keys to whatever holds focus.
fn show_page_buttons(can_turn_pages: bool) -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("LUNCHBOX_HUD_DEBUG_FORCE_PAGE_BUTTONS").is_some() {
        return true;
    }
    can_turn_pages
}

/// One built bar, plus the handles a rebuild needs to take it down again.
///
/// A `GtkPopover` attached with `set_parent` is not owned by its parent the
/// way a box child is: GTK requires it to be unparented explicitly, and warns
/// when a widget is finalized with one still attached. The confirm prompts are
/// additionally rebuilt in place on every scale change, so a rebuild has to
/// read whichever popover is current rather than one captured at build time —
/// hence the `Rc<RefCell<..>>` handles rather than the popovers themselves.
struct HudContent {
    container: gtk4::Box,
    confirm_prompt: std::rc::Rc<std::cell::RefCell<ConfirmPrompt>>,
    reset_prompt: std::rc::Rc<std::cell::RefCell<ConfirmPrompt>>,
    /// The volume and brightness flyouts (issue #178). Rebuilt on a scale
    /// change like the prompts, so the teardown has to read the *current* one
    /// through the cell rather than one captured at build time.
    slider_popovers: [std::rc::Rc<std::cell::RefCell<SliderPopover>>; 2],
    warning_popover: Option<gtk4::Popover>,
}

impl HudContent {
    /// Dismiss and detach everything that would otherwise outlive the bar.
    fn teardown(&self) {
        for prompt in [&self.confirm_prompt, &self.reset_prompt] {
            let prompt = prompt.borrow();
            prompt.popover.popdown();
            prompt.popover.unparent();
        }
        for control in &self.slider_popovers {
            let control = control.borrow();
            control.popover.popdown();
            control.popover.unparent();
        }
        if let Some(popover) = &self.warning_popover {
            popover.popdown();
            popover.unparent();
        }
    }
}

/// The time-remaining warning, in whichever form the bar can hold.
///
/// Horizontally this is the banner it has always been: icon and message side
/// by side in the middle of the bar. Vertically the message has nowhere to go
/// — warning text is operator-authored free prose (`config.example.toml`:
/// "10 minutes left - start wrapping up!"), a 48px bar cannot hold a sentence
/// laid out horizontally, and GTK clips rather than wraps. So the bar keeps
/// the icon, which carries the severity colour and the critical blink on its
/// own, and the message drops out of it as a popover that is as wide as it
/// needs to be. The exclusive zone stays 48px either way.
#[derive(Clone)]
struct WarningBanner {
    /// What sits in the bar.
    container: gtk4::Box,
    icon: gtk4::Image,
    /// The message. In the bar horizontally; inside `popover` vertically.
    label: gtk4::Label,
    /// Present only for the vertical bar.
    popover: Option<gtk4::Popover>,
}

impl WarningBanner {
    fn build(orientation: HudOrientation) -> Self {
        let container = gtk4::Box::builder()
            .orientation(orientation.group())
            .spacing(8)
            .halign(gtk4::Align::Center)
            .visible(false)
            .build();
        container.add_css_class("warning-banner");

        let icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
        container.append(&icon);

        let label = gtk4::Label::new(Some("Time running out!"));
        label.add_css_class("warning-text");

        let popover = if orientation.is_vertical() {
            // Operator-authored prose of no fixed length, so bound it and let
            // it wrap. Without this a long message runs off the right of the
            // screen, where a layer-shell popup is clipped rather than moved.
            label.set_wrap(true);
            label.set_max_width_chars(28);
            // Drops to the right, into the screen, for the same reason the
            // confirmation prompts do — see `align_popover_to_button`.
            let popover = gtk4::Popover::builder()
                .autohide(false)
                .position(gtk4::PositionType::Right)
                .child(&label)
                .build();
            popover.add_css_class("warning-popover");
            popover.set_parent(&container);
            Some(popover)
        } else {
            container.append(&label);
            None
        };

        Self {
            container,
            icon,
            label,
            popover,
        }
    }

    fn set_text(&self, text: &str) {
        self.label.set_text(text);
    }

    /// Show or hide the warning. The popover follows the icon, so a warning
    /// that clears takes its message with it.
    fn set_visible(&self, visible: bool) {
        self.container.set_visible(visible);
        if let Some(popover) = &self.popover {
            if visible {
                popover.popup();
            } else {
                popover.popdown();
            }
        }
    }

    /// Apply the severity styling. The class goes on the bar element in both
    /// layouts, and on the popover too when there is one, so the message is
    /// tinted to match the icon that produced it.
    fn set_severity_class(&self, class: Option<&str>) {
        for target in ["warning-info", "warning-warn", "warning-critical"] {
            self.container.remove_css_class(target);
            if let Some(popover) = &self.popover {
                popover.remove_css_class(target);
            }
        }
        if let Some(class) = class {
            self.container.add_css_class(class);
            if let Some(popover) = &self.popover {
                popover.add_css_class(class);
            }
        }
    }
}

/// The activity name, laid out for whichever bar it lives in.
///
/// Horizontally it is a plain `GtkLabel`; vertically it is the same label
/// turned a quarter turn by [`RotatedLabel`], because GTK4 removed the label
/// `angle` property that would otherwise have done this. Both carry identical
/// styling and identical ellipsize rules — see [`build_title_label`].
#[derive(Clone)]
enum TitleLabel {
    Horizontal(gtk4::Label),
    Vertical(RotatedLabel),
}

impl TitleLabel {
    fn set_text(&self, text: &str) {
        match self {
            Self::Horizontal(label) => label.set_text(text),
            Self::Vertical(label) => label.set_text(text),
        }
    }

    fn widget(&self) -> gtk4::Widget {
        match self {
            Self::Horizontal(label) => label.clone().upcast(),
            Self::Vertical(label) => label.clone().upcast(),
        }
    }

    /// Show or hide the label. Administrator mode's taskbar wants the space the
    /// activity name occupies in the kiosk, and "No session" says nothing there
    /// (issue #154).
    fn set_visible(&self, visible: bool) {
        match self {
            Self::Horizontal(label) => label.set_visible(visible),
            Self::Vertical(label) => label.set_visible(visible),
        }
    }
}

/// Number of characters of activity name the bar guarantees, and the most it
/// will give up to.
///
/// Horizontally these are measured, not guessed: the bar is full at 1280
/// logical pixels, and twelve characters is the most that leaves room for the
/// reading buttons *and* the end-session button. At eighteen the "X" fell off
/// the end, where GTK clips rather than wraps, and a session the child cannot
/// end is a worse failure than a truncated title.
///
/// The vertical bar runs the height of the screen and holds the same widgets,
/// so the same reasoning allows a much longer name before anything is at risk.
const TITLE_CHARS: (i32, i32) = (12, 28);
const VERTICAL_TITLE_CHARS: (i32, i32) = (12, 48);

/// Build the activity-name label with the ellipsize behaviour issue #160
/// settled, in whichever of the two forms this bar needs.
fn build_title_label(orientation: HudOrientation) -> TitleLabel {
    let vertical = orientation.is_vertical();
    let title = if vertical {
        TitleLabel::Vertical(RotatedLabel::new())
    } else {
        TitleLabel::Horizontal(gtk4::Label::new(None))
    };

    // The inner label is a real `GtkLabel` either way, so one setup serves
    // both and the two layouts cannot drift apart.
    let label = match &title {
        TitleLabel::Horizontal(label) => label.clone(),
        TitleLabel::Vertical(rotated) => rotated.label(),
    };
    label.set_text("No session");
    label.add_css_class("app-name");
    // The left box expands, so without this a long activity name ("Alice's
    // Adventures in Wonderland") takes its natural width and pushes the
    // right-hand controls off the end of the bar — where they are simply
    // clipped, not wrapped. Ellipsizing gives the label a small minimum size
    // so the controls always fit (issue #160 added two more of them).
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    // An ellipsizing label asks for the ellipsis as its *minimum*, and GTK
    // hands out minimums unless a child claims the leftover — so without
    // `hexpand` the name collapses to "..." with hundreds of pixels going
    // spare. `xalign` then keeps the text against the start edge as it grows.
    // On the rotated label these are set on the child, whose own axes are
    // still the text's: it expands along the text, and the wrapper turns that
    // into vertical expansion.
    label.set_hexpand(true);
    label.set_xalign(0.0);
    // Bound the request explicitly rather than trusting expand semantics: an
    // ellipsizing label asks for the ellipsis as its minimum and GTK hands out
    // minimums first, so "Alice in Wonderland" rendered as "..." with 400px of
    // the bar unused. A floor keeps the name readable; the ceiling stops a
    // very long one from crowding out the controls it shares the bar with.
    let (min_chars, max_chars) = if vertical {
        VERTICAL_TITLE_CHARS
    } else {
        TITLE_CHARS
    };
    label.set_width_chars(min_chars);
    label.set_max_width_chars(max_chars);

    if let TitleLabel::Vertical(rotated) = &title {
        // The wrapper claims the bar's slack on the bar's own axis; the child
        // above claims it on the text's.
        rotated.set_vexpand(true);
        rotated.set_halign(gtk4::Align::Center);
        rotated.set_valign(gtk4::Align::Fill);
    }

    title
}

/// The HUD application
pub struct HudApp {
    app: gtk4::Application,
    socket_path: PathBuf,
    /// An edge pinned on the command line, which wins over config and makes
    /// the HUD ignore `HudOrientationChanged`. `None` — how `sway.conf` starts
    /// it — follows lunchboxd instead.
    pinned_orientation: Option<HudOrientation>,
    height: i32,
}

impl HudApp {
    pub fn new(
        socket_path: PathBuf,
        pinned_orientation: Option<HudOrientation>,
        height: i32,
    ) -> Self {
        let app = gtk4::Application::builder()
            .application_id("com.lunchboxos.hud")
            .build();

        Self {
            app,
            socket_path,
            pinned_orientation,
            height,
        }
    }

    pub fn run(&self) -> i32 {
        let socket_path = self.socket_path.clone();
        let pinned_orientation = self.pinned_orientation;
        let height = self.height;

        self.app.connect_activate(move |app| {
            let state = SharedState::new();
            let window = build_hud_window(app, pinned_orientation, height, state.clone());

            // Start the IPC event listener
            let state_clone = state.clone();
            let socket_clone = socket_path.clone();
            std::thread::spawn(move || {
                if let Err(e) = run_event_loop(socket_clone, state_clone) {
                    tracing::error!("Event loop error: {}", e);
                }
            });

            // Poll the window list for administrator mode's taskbar (issue
            // #154). Its own connection because `run_event_loop` cannot send
            // RPCs once it has subscribed, and its own thread for the same
            // reason `spawn_action` uses one.
            let state_clone = state.clone();
            let socket_clone = socket_path.clone();
            std::thread::spawn(move || {
                if let Err(e) = run_window_poll(socket_clone, state_clone) {
                    tracing::error!("Window poll error: {}", e);
                }
            });

            // Subscribe to state changes
            let window_clone = window.clone();
            let state_clone = state.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let session_state = state_clone.session_state();
                let visible = session_state.is_visible();
                window_clone.set_visible(visible);
                glib::ControlFlow::Continue
            });

            window.present();
        });

        self.app.run().into()
    }
}

fn build_hud_window(
    app: &gtk4::Application,
    pinned_orientation: Option<HudOrientation>,
    thickness: i32,
    state: SharedState,
) -> gtk4::ApplicationWindow {
    // Unpinned, the bar starts where every device before issue #171 had it and
    // follows lunchboxd from there. On a device configured for a side bar that
    // means a brief top bar at boot, until the first connect seeds the real
    // edge — which is the right trade: a HUD that waits for the daemon before
    // showing itself is a HUD a child cannot end a session from if the daemon
    // is slow or down.
    let orientation = pinned_orientation.unwrap_or_default();
    // `thickness` is the bar's short axis: its height when horizontal, its
    // width when it runs down the side. `apply_scale` sets the corresponding
    // default size and exclusive zone, so nothing is requested here.
    let window = gtk4::ApplicationWindow::builder()
        .application(app)
        .decorated(false)
        .build();

    // CSS provider for the HUD's stylesheet. We install it once and rewrite
    // its contents whenever the UI scale factor changes (see HudScaleChanged
    // and `apply_scale` below) so font/padding sizes follow the factor
    // without needing to reload a fresh provider on the display.
    let css_provider = install_css_provider();

    // Initialize layer shell
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace("lunchbox-hud");

    // Remove all margins from the layer-shell surface
    window.set_margin(Edge::Top, 0);
    window.set_margin(Edge::Bottom, 0);
    window.set_margin(Edge::Left, 0);
    window.set_margin(Edge::Right, 0);

    apply_anchors(&window, orientation);

    // Build the HUD content. apply_scale (below) is responsible for the
    // dynamic dimensions (default height, exclusive zone, font/padding) so
    // they stay in sync with the current UI scale factor.
    let generation = std::rc::Rc::new(std::cell::Cell::new(0_u64));
    let content = std::rc::Rc::new(std::cell::RefCell::new(build_hud_content(
        state.clone(),
        css_provider.clone(),
        window.clone(),
        thickness,
        orientation,
        generation.clone(),
    )));
    window.set_child(Some(&content.borrow().container));

    // Populate the stylesheet and set initial dimensions at scale 1.0
    // before the window maps.
    apply_scale(&css_provider, &window, thickness, 1.0, orientation);

    // Follow lunchboxd's idea of which edge the HUD belongs on (issue #171).
    // The global `[service.hud]` setting applies while the launcher is up; an
    // activity with its own `hud_orientation` moves the bar for the life of
    // its session and it moves back when the session ends.
    let applied_orientation = std::rc::Rc::new(std::cell::Cell::new(orientation));
    let follow_daemon = pinned_orientation.is_none();
    let window_for_orientation = window.clone();
    let css_for_orientation = css_provider.clone();
    let state_for_orientation = state.clone();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        if !follow_daemon {
            return glib::ControlFlow::Continue;
        }
        let desired = state_for_orientation.orientation();
        if desired == applied_orientation.get() {
            return glib::ControlFlow::Continue;
        }
        tracing::info!(
            before = ?applied_orientation.get(),
            after = ?desired,
            "Rebuilding the HUD for a new orientation"
        );
        applied_orientation.set(desired);

        // The bar is rebuilt rather than restyled, for the reason issue #118
        // documents: GTK validates a widget's style when it is *mapped* and
        // leaves it alone while hidden, so anything currently hidden — the
        // confirm prompts, the warning — would keep the previous layout's
        // sizes and paint at them the next time it is shown. A fresh widget
        // has no cached style. Rebuilding also spares every widget below from
        // having to know how to change its own axis.
        //
        // Bumping the generation first is what retires the old bar's 500ms
        // update timer: it sees a generation that is no longer its own on its
        // next tick and stops, so two timers never drive the HUD at once.
        generation.set(generation.get() + 1);
        content.borrow().teardown();

        let rebuilt = build_hud_content(
            state_for_orientation.clone(),
            css_for_orientation.clone(),
            window_for_orientation.clone(),
            thickness,
            desired,
            generation.clone(),
        );
        window_for_orientation.set_child(Some(&rebuilt.container));
        *content.borrow_mut() = rebuilt;

        // Changing anchors on a surface the compositor has already mapped does
        // not move it; the layer surface has to be built again. Same unmap →
        // reconfigure → remap dance the output switch uses, and for the same
        // reason (see the `set_monitor` call in the update timer).
        let visible = window_for_orientation.is_visible();
        window_for_orientation.set_visible(false);
        apply_anchors(&window_for_orientation, desired);
        apply_scale(
            &css_for_orientation,
            &window_for_orientation,
            thickness,
            state_for_orientation.scale_factor(),
            desired,
        );
        window_for_orientation.set_visible(visible);

        glib::ControlFlow::Continue
    });

    window
}

/// Anchor the layer surface to the edge the bar sits on plus the two it spans.
///
/// Spanning is what makes the surface stretch the full length of its edge and
/// gives the compositor an exclusive zone to reserve on the fourth. Every
/// anchor is set explicitly, including the ones being turned *off*, because
/// this is called again on an orientation change and a stale anchor left
/// behind would pin the bar to two opposite edges at once.
fn apply_anchors(window: &gtk4::ApplicationWindow, orientation: HudOrientation) {
    let (top, bottom, left, right) = match orientation {
        HudOrientation::Top => (true, false, true, true),
        HudOrientation::Bottom => (false, true, true, true),
        HudOrientation::Left => (true, true, true, false),
    };
    window.set_anchor(Edge::Top, top);
    window.set_anchor(Edge::Bottom, bottom);
    window.set_anchor(Edge::Left, left);
    window.set_anchor(Edge::Right, right);
}

fn build_hud_content(
    state: SharedState,
    css_provider: gtk4::CssProvider,
    window: gtk4::ApplicationWindow,
    base_thickness: i32,
    orientation: HudOrientation,
    generation: std::rc::Rc<std::cell::Cell<u64>>,
) -> HudContent {
    // Every box below runs along the bar, and every `flow_append` puts its
    // child at the far end of it — which for a bar rotated to the left means
    // the top of the screen. See `HudOrientation::flow_append`.
    let vertical = orientation.is_vertical();
    let container = gtk4::Box::builder()
        .orientation(orientation.flow())
        .spacing(16)
        .hexpand(!vertical)
        .vexpand(vertical)
        .build();

    container.add_css_class("hud-bar");
    if vertical {
        // Lets the stylesheet swap the bar's padding onto the other axis and
        // shrink the readouts that have to fit across 48px rather than along
        // it. Everything else is the same rule for both layouts.
        container.add_css_class("hud-vertical");
    }

    // Left section: App name and time
    // `halign(Fill)`, not `Start`: with `Start` the box is allocated its
    // *minimum* width and merely positioned left, which collapses the
    // ellipsizing label below to the ellipsis even when the bar has hundreds
    // of pixels to spare. Filling gives the name the leftover room, and the
    // ellipsis then only appears when the bar is genuinely full.
    let left_box = gtk4::Box::builder()
        .orientation(orientation.flow())
        .spacing(12)
        .hexpand(!vertical)
        .vexpand(vertical)
        .halign(gtk4::Align::Fill)
        .valign(gtk4::Align::Fill)
        .build();

    // Page-turn buttons, shown only for activities that read (issue #160).
    // A reader turns pages on a key, a D-pad or a wheel; a touchscreen
    // produces none of those and has no swipe gesture to fall back on, so on a
    // touch-only panel these are the only way through a book. They sit at the
    // far left of the bar, as far as it is possible to be from the reset and
    // end-session buttons on the right: the two controls a child uses on every
    // page should not share an edge with the two that throw the session away.
    //
    // They live in a box of their own so the pair stays together and in
    // reading order when the bar is reversed for the vertical layout: the box
    // lands at the bottom as a unit, with "back" still ahead of "forward".
    //
    // The arrows point the way the *pages* go, not the way the buttons are
    // stacked: `‹` back and `›` forward in both layouts. The vertical bar used
    // to swap them for `⌃`/`⌄`, on the reasoning that a sideways arrow means
    // nothing in a column — but the direction a reader thinks in is the page's,
    // and it does not rotate when the bar does. A child who learns `›` on one
    // device should not have to learn it again on another.
    let page_box = gtk4::Box::builder()
        .orientation(orientation.group())
        .spacing(0)
        .visible(false)
        .build();

    let page_back_icon = gtk4::Image::from_icon_name("go-previous-symbolic");
    page_back_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let page_back_button = gtk4::Button::builder()
        .child(&page_back_icon)
        .has_frame(false)
        .tooltip_text("Previous page")
        .build();
    page_back_button.add_css_class("indicator-button");
    page_back_button.add_css_class("page-button");
    page_box.append(&page_back_button);

    let page_forward_icon = gtk4::Image::from_icon_name("go-next-symbolic");
    page_forward_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let page_forward_button = gtk4::Button::builder()
        .child(&page_forward_icon)
        .has_frame(false)
        .tooltip_text("Next page")
        .build();
    page_forward_button.add_css_class("indicator-button");
    page_forward_button.add_css_class("page-button");
    page_box.append(&page_forward_button);

    orientation.flow_append(&left_box, &page_box);

    let app_label = build_title_label(orientation);
    orientation.flow_append(&left_box, &app_label.widget());

    let time_display = TimeDisplay::new();
    time_display.set_compact(vertical);
    orientation.flow_append(&left_box, &time_display);

    // Administrator mode's taskbar (issue #154). Hidden in the kiosk, where the
    // whole point is that there is nothing to switch between; shown when a
    // caregiver is setting the device up, where there is.
    //
    // Only the window buttons are rebuilt as windows come and go — the Start
    // button is permanent, so it keeps its click handler across refreshes.
    let taskbar_box = gtk4::Box::builder()
        .orientation(orientation.group())
        .spacing(6)
        .visible(false)
        .build();

    // The Start button raises the launcher, which in administrator mode *is*
    // the app picker. Nothing new to show or hide: focusing its window brings
    // the picker in front of whatever the caregiver has open.
    let start_button = gtk4::Button::builder()
        .label("Apps")
        .tooltip_text("Show the application picker")
        // Frameless like every other control on this bar; the default frame is
        // a light rounded rect that reads as a blank tile against the HUD.
        .has_frame(false)
        .build();
    start_button.add_css_class("indicator-button");
    let state_for_start = state.clone();
    start_button.connect_clicked(move |_| {
        // In administrator mode the launcher *is* the picker, so "Apps" is
        // simply "raise the launcher". Reuses `WindowAction::Focus` rather than
        // inventing an RPC: the taskbar already polls the window list, so the
        // id is in hand.
        let Some(id) = shell_window_id(&state_for_start.windows()) else {
            tracing::warn!("Apps pressed but the launcher's window was not in the list");
            return;
        };
        spawn_action(
            default_socket_path(),
            "focus launcher",
            move |mut client| async move {
                client
                    .act_on_window(id, lunchbox_api::WindowAction::Focus)
                    .await
            },
        );
    });
    taskbar_box.append(&start_button);

    let window_buttons = gtk4::Box::builder()
        .orientation(orientation.group())
        .spacing(6)
        .build();
    taskbar_box.append(&window_buttons);
    orientation.flow_append(&left_box, &taskbar_box);

    orientation.flow_append(&container, &left_box);

    // Center section: Warning banner (hidden by default)
    let warning = WarningBanner::build(orientation);
    let warning_box = warning.container.clone();
    let warning_icon = warning.icon.clone();
    orientation.flow_append(&container, &warning_box);

    // Right section: System indicators and close button
    let right_box = gtk4::Box::builder()
        .orientation(orientation.flow())
        .spacing(8)
        .halign(if vertical {
            gtk4::Align::Center
        } else {
            gtk4::Align::End
        })
        .valign(if vertical {
            gtk4::Align::Start
        } else {
            gtk4::Align::Center
        })
        .build();

    // Wall clock display (shows mock time indicator in debug builds)
    let clock_box = gtk4::Box::builder()
        .orientation(orientation.group())
        .spacing(4)
        .build();

    // `HH:MM` is wider than a 48px bar can hold, and a clock read sideways is
    // worse than none — so the vertical bar shows a round face instead, which
    // is the one form of a clock as wide as it is tall (issue #171).
    let analog_clock = vertical.then(crate::analog_clock::build);
    let clock_label = gtk4::Label::new(Some("--:--"));
    clock_label.add_css_class("clock-label");
    match &analog_clock {
        Some(face) => clock_box.append(face),
        None => clock_box.append(&clock_label),
    }
    let mut clock_format_full = false;

    // Add mock indicator if mock time is active (debug builds only)
    #[cfg(debug_assertions)]
    {
        if lunchbox_util::is_mock_time_active() {
            let mock_indicator = gtk4::Label::new(Some("(MOCK)"));
            mock_indicator.add_css_class("mock-time-indicator");
            clock_box.append(&mock_indicator);
            clock_format_full = true;
        }
    }

    orientation.flow_append(&right_box, &clock_box);

    // Volume. The bar carries the icon; the slider and the mute toggle open
    // out of it as a flyout (issue #178) — see `SliderPopover` for why they
    // are no longer in the bar. An explicit child Image, so its pixel size
    // follows the HUD scale factor (see `apply_scale`); `Button::set_icon_name`
    // would replace the child, so the timer below updates `volume_icon`
    // directly via `set_from_icon_name`.
    let volume_icon = gtk4::Image::from_icon_name("audio-volume-medium-symbolic");
    volume_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let volume_button = gtk4::Button::builder()
        .child(&volume_icon)
        .has_frame(false)
        .tooltip_text("Volume")
        .build();
    volume_button.add_css_class("indicator-button");

    // Handle slider value changes with debouncing
    // Create a channel for volume requests - the worker will debounce them
    let (volume_tx, volume_rx) = mpsc::channel::<u8>();

    // Spawn a dedicated volume worker thread that debounces requests
    std::thread::spawn(move || {
        const DEBOUNCE_MS: u64 = 50; // Wait 50ms for more changes before sending

        while let Ok(mut latest_percent) = volume_rx.recv() {
            // Drain any pending requests, keeping only the latest value
            // Use a short timeout to debounce rapid changes
            loop {
                match volume_rx.recv_timeout(std::time::Duration::from_millis(DEBOUNCE_MS)) {
                    Ok(percent) => {
                        latest_percent = percent; // Update to latest value
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // No more changes for DEBOUNCE_MS, send the request
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return; // Channel closed
                    }
                }
            }

            // Send only the final value
            if let Err(e) = crate::volume::set_volume(latest_percent) {
                tracing::error!("Failed to set volume: {}", e);
            }
        }
    });

    // Set while the child is dragging the flyout's slider, so the update loop
    // does not yank the knob back to the last value the daemon reported.
    let slider_changing = std::rc::Rc::new(std::cell::Cell::new(false));

    let volume_popover = std::rc::Rc::new(std::cell::RefCell::new(build_volume_popover(
        &volume_button,
        &window,
        1.0,
        orientation,
        &volume_tx,
        &slider_changing,
    )));

    orientation.flow_append(&right_box, &volume_button);

    // Brightness. Same shape as volume: the bar carries the icon and the
    // controls fly out of it. Hidden when the host has no backlight (every
    // desktop machine, plus laptops missing `/sys/class/backlight/*`).
    //
    // The icon used to *be* the automatic-brightness toggle. That toggle moves
    // into the flyout, where it can say what it is; the bar icon keeps
    // reporting the state by lighting up in the brightness bar's own colour
    // when the user has taken manual control (`.brightness-manual`, applied by
    // the update loop), so nothing is lost from a glance at the bar.
    let brightness_icon = gtk4::Image::from_icon_name("display-brightness-symbolic");
    brightness_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let brightness_button = gtk4::Button::builder()
        .child(&brightness_icon)
        .has_frame(false)
        .tooltip_text("Brightness")
        .visible(false)
        .build();
    brightness_button.add_css_class("indicator-button");

    // Debounce brightness changes the same way as volume: the slider can
    // emit dozens of events per second while the user drags it, and the
    // sysfs/`brightnessctl` write is fast but not free.
    let (brightness_tx, brightness_rx_chan) = mpsc::channel::<u8>();
    std::thread::spawn(move || {
        const DEBOUNCE_MS: u64 = 50;

        while let Ok(mut latest_percent) = brightness_rx_chan.recv() {
            loop {
                match brightness_rx_chan.recv_timeout(std::time::Duration::from_millis(DEBOUNCE_MS))
                {
                    Ok(percent) => {
                        latest_percent = percent;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }

            if let Err(e) = crate::brightness::set_brightness(latest_percent) {
                tracing::error!("Failed to set brightness: {}", e);
            }
        }
    });

    let brightness_changing = std::rc::Rc::new(std::cell::Cell::new(false));

    let brightness_popover = std::rc::Rc::new(std::cell::RefCell::new(build_brightness_popover(
        &brightness_button,
        &window,
        1.0,
        orientation,
        &brightness_tx,
        &brightness_changing,
    )));

    orientation.flow_append(&right_box, &brightness_button);

    // Display mode toggle (issue #87): mirror ⇄ external-only. Hidden unless an
    // external display is connected. Uses an explicit child Image so its pixel
    // size follows the HUD scale factor, like the other indicator buttons.
    let display_icon = gtk4::Image::from_icon_name("preferences-desktop-display-symbolic");
    display_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let display_button = gtk4::Button::builder()
        .child(&display_icon)
        .has_frame(false)
        .tooltip_text("Toggle external display mode")
        .visible(false)
        .build();
    display_button.add_css_class("indicator-button");
    let state_for_display = state.clone();
    display_button.connect_clicked(move |_| {
        if let Some(ds) = state_for_display.display_state() {
            let target = ds.mode.toggled();
            spawn_action(
                default_socket_path(),
                "set_display_mode",
                move |mut client| async move { client.set_display_mode(target).await.map(|_| ()) },
            );
        }
    });
    orientation.flow_append(&right_box, &display_button);

    // Lock button (issue #154). Present only in administrator mode, where it is
    // also the one thing on the device's own screen saying the mode is on.
    //
    // Locking is offered here; unlocking deliberately is not. The screen can
    // only be opened again from the companion or web app, which is what makes
    // it safe to lock a half-configured device and walk away from it.
    let lock_icon = gtk4::Image::from_icon_name("system-lock-screen-symbolic");
    lock_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let lock_button = gtk4::Button::builder()
        .child(&lock_icon)
        .has_frame(false)
        .tooltip_text("Lock the screen (unlock from the Lunchbox app)")
        .visible(false)
        .build();
    lock_button.add_css_class("indicator-button");
    lock_button.connect_clicked(move |_| {
        spawn_action(
            default_socket_path(),
            "lock_device",
            move |mut client| async move { client.lock_device().await },
        );
    });
    right_box.append(&lock_button);

    // Network connectivity indicator. Shown only when at least one
    // connectivity check is configured. Icon reflects the worst status across
    // all configured checks; the tooltip lists every check and its result so
    // operators can see which target failed.
    let network_box = gtk4::Box::builder()
        .orientation(orientation.group())
        .spacing(4)
        .visible(false)
        .build();
    network_box.add_css_class("network-indicator");

    let network_icon = gtk4::Image::from_icon_name("network-offline-symbolic");
    network_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    network_box.append(&network_icon);

    orientation.flow_append(&right_box, &network_box);

    // Battery indicator
    let battery_box = gtk4::Box::builder()
        .orientation(orientation.group())
        .spacing(4)
        .build();

    let battery_icon = gtk4::Image::from_icon_name("battery-good-symbolic");
    battery_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    battery_box.append(&battery_icon);

    let battery_label = gtk4::Label::new(Some("--%"));
    battery_label.add_css_class("battery-label");
    battery_box.append(&battery_label);

    orientation.flow_append(&right_box, &battery_box);

    // Reset ("reboot the console") button, shown only for activities that
    // support it (issue #125). With RetroArch's save-state resume on, every
    // launch puts the child back exactly where they stopped, so this is the
    // only way back to a game's own title screen.
    let reset_icon = gtk4::Image::from_icon_name("view-refresh-symbolic");
    reset_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let reset_button = gtk4::Button::builder()
        .child(&reset_icon)
        .has_frame(false)
        .tooltip_text("Restart activity")
        .visible(false)
        .build();
    reset_button.add_css_class("indicator-button");
    orientation.flow_append(&right_box, &reset_button);

    // Action button: shows as "End session" when a session is active, "Log out" otherwise.
    // Uses an explicit child Image for the same reason as `volume_button`.
    let action_icon = gtk4::Image::from_icon_name("system-log-out-symbolic");
    action_icon.set_pixel_size(BASE_ICON_PIXEL_SIZE);
    let action_button = gtk4::Button::builder()
        .child(&action_icon)
        .has_frame(false)
        .tooltip_text("Log out")
        .build();
    action_button.add_css_class("close-button");

    // Confirmation prompt for the "X" button (issue #78). Built for the current
    // scale factor and rebuilt whenever it changes — see `build_confirm_prompt`.
    let confirm_prompt = std::rc::Rc::new(std::cell::RefCell::new(build_confirm_prompt(
        &action_button,
        &window,
        1.0,
        ConfirmAction::EndActivity,
        orientation,
    )));
    // The reset button gets its own prompt, parented to its own button so it
    // drops from the right place. Same rebuild-on-scale-change rules apply.
    let reset_prompt = std::rc::Rc::new(std::cell::RefCell::new(build_confirm_prompt(
        &reset_button,
        &window,
        1.0,
        ConfirmAction::ResetActivity,
        orientation,
    )));

    let state_for_action = state.clone();
    let prompt_for_action = confirm_prompt.clone();
    let window_for_action = window.clone();
    action_button.connect_clicked(move |btn| {
        let session_state = state_for_action.session_state();
        let socket_path = default_socket_path();
        if session_state.session_id().is_some() {
            if session_state.confirm_on_close() {
                // Take our own references and drop the borrow before popping:
                // `popup()` runs signal handlers, and one of them reaching back
                // into the cell would panic.
                let (popover, content, label) = {
                    let prompt = prompt_for_action.borrow();
                    (
                        prompt.popover.clone(),
                        prompt.content.clone(),
                        prompt.label.clone(),
                    )
                };
                // Refresh the prompt with the activity's name, then ask. Take
                // keyboard focus so the autohide grab can dismiss on focus loss.
                if let Some(name) = session_state.entry_name() {
                    label.set_text(&format!("End {name}? Unsaved progress may be lost."));
                } else {
                    label.set_text("End this activity? Unsaved progress may be lost.");
                }
                window_for_action.set_keyboard_mode(KeyboardMode::OnDemand);
                // Right-align the popover to the button (issue #97).
                align_popover_to_button(
                    &popover,
                    &content,
                    btn,
                    state_for_action.scale_factor(),
                    orientation,
                );
                popover.popup();
            } else {
                request_stop_current(socket_path);
            }
        } else if state_for_action.admin_mode() {
            // Administrator mode's "X" closes the focused window, and only
            // becomes "leave the mode" once none are left (the issue's own
            // design). The gate is deliberately local to this button: the
            // management clients can always leave, which is what rescues a
            // window that refuses to close.
            let windows = admin_windows_on_screen(&state_for_action.windows());
            match windows
                .iter()
                .find(|w| w.focused)
                .or_else(|| windows.first())
            {
                Some(w) => {
                    let id = w.id;
                    spawn_action(socket_path, "close window", move |mut client| async move {
                        client
                            .act_on_window(id, lunchbox_api::WindowAction::Close)
                            .await
                    });
                }
                None => {
                    // Leaving logs the session out (issue #154), so this HUD is
                    // one of the things about to go away. Nothing to do about
                    // that here — the daemon drains its clients before it tears
                    // sway down — but it is why the button says "log out".
                    tracing::info!("Leaving administrator mode; the session will end");
                    spawn_action(socket_path, "exit_admin_mode", |mut client| async move {
                        client.exit_admin_mode().await
                    });
                }
            }
        } else {
            tracing::info!("Requesting logout");
            spawn_action(socket_path, "logout", |mut client| async move {
                client.logout().await
            });
        }
    });
    orientation.flow_append(&right_box, &action_button);

    // Open the flyouts from the bar icons (issue #178). Both read the current
    // popover through the cell rather than one captured here, because a scale
    // change rebuilds them (see `SliderPopover`).
    for (button, control) in [
        (&volume_button, &volume_popover),
        (&brightness_button, &brightness_popover),
    ] {
        let control = control.clone();
        let window_for_open = window.clone();
        let state_for_open = state.clone();
        button.connect_clicked(move |btn| {
            open_slider_popover(
                &control,
                btn,
                &window_for_open,
                state_for_open.scale_factor(),
                orientation,
            );
        });
    }

    // One virtual keyboard for the life of the HUD, created on the first
    // press. See `page_turn` for why the key rather than an RPC.
    let page_turner = crate::page_turn::PageTurner::new();
    for (button, direction) in [
        (&page_back_button, crate::page_turn::Direction::Back),
        (&page_forward_button, crate::page_turn::Direction::Forward),
    ] {
        let state_for_page = state.clone();
        let turner = page_turner.clone();
        button.connect_clicked(move |_| {
            // The visibility check is not enough on its own: a session can end
            // between the press and the handler, and a key sent then would
            // land on the launcher.
            if !state_for_page.session_state().can_turn_pages() {
                return;
            }
            turner.borrow_mut().turn(direction);
        });
    }

    let state_for_reset = state.clone();
    let prompt_for_reset = reset_prompt.clone();
    let window_for_reset = window.clone();
    reset_button.connect_clicked(move |btn| {
        let session_state = state_for_reset.session_state();
        if !session_state.can_reset() {
            return;
        }
        // Same borrow discipline as the close prompt: take clones and drop the
        // borrow before `popup()` runs handlers that may reach back in.
        let (popover, content, label) = {
            let prompt = prompt_for_reset.borrow();
            (
                prompt.popover.clone(),
                prompt.content.clone(),
                prompt.label.clone(),
            )
        };
        label.set_text(&ConfirmAction::ResetActivity.message(session_state.entry_name()));
        window_for_reset.set_keyboard_mode(KeyboardMode::OnDemand);
        align_popover_to_button(
            &popover,
            &content,
            btn,
            state_for_reset.scale_factor(),
            orientation,
        );
        popover.popup();
    });

    // The generation this bar was built as. An orientation change bumps the
    // shared counter and builds a new bar; every timer below notices on its
    // next tick and retires, so a replaced bar's widgets stop being driven and
    // two bars never fight over the same window.
    let my_generation = generation.get();

    // Debug-build test hook for the headless dev harness, which has no way to
    // click a GTK button (the synthetic pointer does not fire `clicked`; see the
    // `headless-dev` skill). With `LUNCHBOX_HUD_DEBUG_CONFIRM_TRIGGER=<path>`
    // set, creating `<path>` pops the close-confirmation prompt, `<path>.reset`
    // pops the reset one, `<path>.volume` / `<path>.brightness` open the two
    // pop-out controls, `<path>.down` dismisses whichever is up, and
    // `<path>.page_next` / `<path>.page_prev` press the page-turn buttons;
    // every file is consumed. That is enough to drive open/close cycles — and scale
    // changes across them — from a shell. Never compiled into a release build.
    #[cfg(debug_assertions)]
    if let Ok(trigger) = std::env::var("LUNCHBOX_HUD_DEBUG_CONFIRM_TRIGGER") {
        let up = std::path::PathBuf::from(&trigger);
        let up_reset = std::path::PathBuf::from(format!("{trigger}.reset"));
        let down = std::path::PathBuf::from(format!("{trigger}.down"));
        let volume_up = std::path::PathBuf::from(format!("{trigger}.volume"));
        let brightness_up = std::path::PathBuf::from(format!("{trigger}.brightness"));
        let page_next = std::path::PathBuf::from(format!("{trigger}.page_next"));
        let page_prev = std::path::PathBuf::from(format!("{trigger}.page_prev"));
        let volume_for_debug = volume_button.clone();
        let brightness_for_debug = brightness_button.clone();
        let page_forward_for_debug = page_forward_button.clone();
        let page_back_for_debug = page_back_button.clone();
        let action_button_for_debug = action_button.clone();
        let reset_button_for_debug = reset_button.clone();
        let prompt_for_debug = confirm_prompt.clone();
        let reset_prompt_for_debug = reset_prompt.clone();
        let volume_popover_for_debug = volume_popover.clone();
        let brightness_popover_for_debug = brightness_popover.clone();
        let generation_for_debug = generation.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            // Retire with the bar this hook was built for, like the update
            // timer above; otherwise an orientation change would leave one
            // trigger watcher per bar ever built, all firing at once.
            if generation_for_debug.get() != my_generation {
                return glib::ControlFlow::Break;
            }
            if up.exists() {
                let _ = std::fs::remove_file(&up);
                action_button_for_debug.emit_clicked();
            }
            if up_reset.exists() {
                let _ = std::fs::remove_file(&up_reset);
                reset_button_for_debug.emit_clicked();
            }
            if volume_up.exists() {
                let _ = std::fs::remove_file(&volume_up);
                volume_for_debug.emit_clicked();
            }
            if brightness_up.exists() {
                let _ = std::fs::remove_file(&brightness_up);
                brightness_for_debug.emit_clicked();
            }
            if down.exists() {
                let _ = std::fs::remove_file(&down);
                prompt_for_debug.borrow().popover.popdown();
                reset_prompt_for_debug.borrow().popover.popdown();
                for control in [&volume_popover_for_debug, &brightness_popover_for_debug] {
                    control.borrow().popover.popdown();
                }
            }
            if page_next.exists() {
                let _ = std::fs::remove_file(&page_next);
                page_forward_for_debug.emit_clicked();
            }
            if page_prev.exists() {
                let _ = std::fs::remove_file(&page_prev);
                page_back_for_debug.emit_clicked();
            }
            glib::ControlFlow::Continue
        });
    }

    orientation.flow_append(&container, &right_box);

    // Set up state updates
    let app_label_clone = app_label.clone();
    let time_display_clone = time_display.clone();
    let warning_for_timer = warning.clone();
    let battery_box_clone = battery_box.clone();
    let battery_icon_clone = battery_icon.clone();
    let battery_label_clone = battery_label.clone();
    let volume_button_clone = volume_button.clone();
    let volume_icon_clone = volume_icon.clone();
    let volume_popover_for_timer = volume_popover.clone();
    let slider_changing_for_update = slider_changing.clone();
    let brightness_icon_clone = brightness_icon.clone();
    let brightness_popover_for_timer = brightness_popover.clone();
    let brightness_changing_for_update = brightness_changing.clone();
    let brightness_button_clone = brightness_button.clone();
    // Handles the scale-change rebuild needs to construct fresh flyouts.
    let volume_button_for_rebuild = volume_button.clone();
    let brightness_button_for_rebuild = brightness_button.clone();
    let volume_tx_for_rebuild = volume_tx.clone();
    let brightness_tx_for_rebuild = brightness_tx.clone();
    let slider_changing_for_rebuild = slider_changing.clone();
    let brightness_changing_for_rebuild = brightness_changing.clone();
    let clock_label_clone = clock_label.clone();
    let action_button_clone = action_button.clone();
    let action_icon_clone = action_icon.clone();
    let confirm_prompt_for_timer = confirm_prompt.clone();
    let action_button_for_rebuild = action_button.clone();
    let reset_button_clone = reset_button.clone();
    let page_box_clone = page_box.clone();
    let analog_clock_for_timer = analog_clock.clone();
    let analog_clock_for_scale = analog_clock.clone();
    let reset_prompt_for_timer = reset_prompt.clone();
    let reset_button_for_rebuild = reset_button.clone();
    let window_for_rebuild = window.clone();
    let network_box_clone = network_box.clone();
    let network_icon_clone = network_icon.clone();
    let display_button_clone = display_button.clone();
    let lock_button_clone = lock_button.clone();
    let taskbar_box_clone = taskbar_box.clone();
    let window_buttons_clone = window_buttons.clone();
    // Tracks the connector the HUD is currently anchored to, so we only
    // re-anchor the layer-shell surface when the active output actually changes.
    let anchored_connector = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let window_for_monitor = window.clone();
    // All icons we resize when the HUD scale factor changes. Every `Image` the
    // bar owns has to be listed: an icon's pixel size is a widget property, so
    // `scale_px_literals` never reaches it and one left out here renders
    // 1/factor too small next to neighbours that grew (issue #114). The
    // page-turn pair were missed when they were added in #160 — latent, since a
    // reading activity is unlikely to be `xwayland_native_resolution`, but
    // wrong by the same rule.
    let scaled_icons: [gtk4::Image; 9] = [
        warning_icon.clone(),
        battery_icon.clone(),
        volume_icon.clone(),
        brightness_icon.clone(),
        action_icon.clone(),
        network_icon.clone(),
        display_icon.clone(),
        page_back_icon.clone(),
        page_forward_icon.clone(),
    ];
    // Every `gtk4::Box` in the HUD, with the spacing it uses at factor 1.0.
    // Box spacing is a widget property rather than CSS, so `scale_px_literals`
    // never reaches it: left alone it keeps its logical-pixel value and the
    // counter-scaled HUD comes out visibly tighter than the same UI on an
    // un-hacked HiDPI panel — most obviously in the close-confirmation prompt,
    // whose whole surface then measures short (issue #118). Rescaling these
    // alongside the icons and sliders closes the gap the #114 fix left open.
    // The flyouts' own rows are absent on purpose: they are rebuilt for the
    // new factor rather than rescaled in place (see `SliderPopover`), so they
    // are constructed with the right spacing already.
    let scaled_boxes: [(gtk4::Box, i32); 7] = [
        (container.clone(), 16),
        (left_box.clone(), 12),
        (warning_box.clone(), 8),
        (right_box.clone(), 8),
        (clock_box.clone(), 4),
        (network_box.clone(), 4),
        (battery_box.clone(), 4),
    ];
    // Track the most-recently-applied scale factor so we only rebuild the
    // stylesheet when lunchboxd sends a new HudScaleChanged value.
    let applied_scale = std::rc::Rc::new(std::cell::Cell::new(1.0_f64));
    let applied_scale_for_timer = applied_scale.clone();
    let css_provider_for_timer = css_provider.clone();
    let window_for_timer = window.clone();

    glib::timeout_add_local(Duration::from_millis(500), move || {
        if generation.get() != my_generation {
            return glib::ControlFlow::Break;
        }

        // Re-apply scaling if lunchboxd has changed it since the last tick.
        // The HUD bar height, exclusive zone, and stylesheet all derive from
        // this factor.
        let desired_scale = state.scale_factor();
        if (desired_scale - applied_scale_for_timer.get()).abs() > f64::EPSILON {
            apply_scale(
                &css_provider_for_timer,
                &window_for_timer,
                base_thickness,
                desired_scale,
                orientation,
            );
            let icon_size = (f64::from(BASE_ICON_PIXEL_SIZE) * desired_scale).round() as i32;
            for icon in &scaled_icons {
                icon.set_pixel_size(icon_size);
            }
            if let Some(face) = &analog_clock_for_scale {
                crate::analog_clock::set_diameter(
                    face,
                    (f64::from(crate::analog_clock::BASE_CLOCK_DIAMETER) * desired_scale).round()
                        as i32,
                );
            }
            for (boxed, base_spacing) in &scaled_boxes {
                boxed.set_spacing((f64::from(*base_spacing) * desired_scale).round() as i32);
            }
            // Rebuild the close-confirmation prompt for the new factor. It is
            // hidden right now, and GTK does not restyle hidden widgets, so the
            // one built for the previous factor would keep that factor's sizes
            // — the failure where the bar is correct but the prompt renders
            // un-counter-scaled (issue #118). Must come after `apply_scale`, so
            // the fresh widgets pick up the stylesheet it just loaded.
            {
                let mut prompt = confirm_prompt_for_timer.borrow_mut();
                prompt.popover.popdown();
                prompt.popover.unparent();
                *prompt = build_confirm_prompt(
                    &action_button_for_rebuild,
                    &window_for_rebuild,
                    desired_scale,
                    ConfirmAction::EndActivity,
                    orientation,
                );
            }
            {
                let mut prompt = reset_prompt_for_timer.borrow_mut();
                prompt.popover.popdown();
                prompt.popover.unparent();
                *prompt = build_confirm_prompt(
                    &reset_button_for_rebuild,
                    &window_for_rebuild,
                    desired_scale,
                    ConfirmAction::ResetActivity,
                    orientation,
                );
            }
            // The flyouts go the same way, and for the same reason: they live
            // hidden across the change and are *measured* before being shown
            // (issue #178 built them on the #118 rule deliberately). The state
            // they carry is re-pushed further down this same tick.
            {
                let mut control = volume_popover_for_timer.borrow_mut();
                control.popover.popdown();
                control.popover.unparent();
                *control = build_volume_popover(
                    &volume_button_for_rebuild,
                    &window_for_rebuild,
                    desired_scale,
                    orientation,
                    &volume_tx_for_rebuild,
                    &slider_changing_for_rebuild,
                );
            }
            {
                let mut control = brightness_popover_for_timer.borrow_mut();
                control.popover.popdown();
                control.popover.unparent();
                *control = build_brightness_popover(
                    &brightness_button_for_rebuild,
                    &window_for_rebuild,
                    desired_scale,
                    orientation,
                    &brightness_tx_for_rebuild,
                    &brightness_changing_for_rebuild,
                );
            }
            applied_scale_for_timer.set(desired_scale);
        }

        // While suspending (or awaiting fresh state on resume) the time,
        // battery, and network indicators show placeholders instead of live
        // values, so the frame frozen across the suspend/resume gap is never a
        // stale status (issue #73).
        let suspended = state.is_suspended();

        // Update wall clock display. The analog face reads the clock in its own
        // draw function, so it only needs to be told that something changed --
        // but it still has to be told, or it would keep the frame it first
        // rendered for the life of the session.
        if let Some(face) = &analog_clock_for_timer {
            face.set_visible(!suspended);
            if !suspended {
                face.queue_draw();
            }
        } else if suspended {
            clock_label_clone.set_text("--:--");
        } else {
            let current_time = lunchbox_util::now();
            if clock_format_full {
                clock_label_clone.set_text(&lunchbox_util::format_datetime_full(&current_time));
            } else {
                clock_label_clone.set_text(&lunchbox_util::format_clock_time(&current_time));
            }
        }

        let admin_mode = state.admin_mode();

        // Update session state
        let session_state = state.session_state();
        let has_session = session_state.session_id().is_some();
        if has_session {
            action_icon_clone.set_icon_name(Some("window-close-symbolic"));
            action_button_clone.set_tooltip_text(Some("End session"));
        } else if admin_mode {
            // The issue's design: the button closes windows until there are
            // none, then changes to the way out of the mode.
            if admin_windows_on_screen(&state.windows()).is_empty() {
                action_icon_clone.set_icon_name(Some("system-log-out-symbolic"));
                // Says "log out" because it is one: leaving the mode ends the
                // session, which is how anything the caregiver started stops
                // following the child around.
                action_button_clone.set_tooltip_text(Some("Leave administrator mode and log out"));
            } else {
                action_icon_clone.set_icon_name(Some("window-close-symbolic"));
                action_button_clone.set_tooltip_text(Some("Close the focused window"));
            }
        } else {
            action_icon_clone.set_icon_name(Some("system-log-out-symbolic"));
            action_button_clone.set_tooltip_text(Some("Log out"));
        }
        // If the activity has ended or started ending by any means other than
        // the prompt itself (time expiry, API stop, process exit), dismiss a
        // lingering close-confirmation popover — there is nothing left to
        // confirm (issue #78). Harmless no-op when it isn't showing.
        if !matches!(
            session_state,
            SessionState::Active { .. } | SessionState::Warning { .. }
        ) {
            confirm_prompt_for_timer.borrow().popover.popdown();
        }
        // The reset button belongs to the activity, so it appears and
        // disappears with one that supports being reset -- and its prompt goes
        // with it, for the same reason the close prompt does.
        let can_reset = session_state.can_reset();
        reset_button_clone.set_visible(can_reset);
        if !can_reset {
            reset_prompt_for_timer.borrow().popover.popdown();
        }
        // Same rule for the page buttons: they belong to the activity, so a
        // session that is not a reading one never shows them.
        // Showing them costs the bar nothing it has to take back any more.
        // Until issue #178 the two buttons had to be paid for out of the
        // sliders' length and the numeric readouts, because the sliders were
        // in the bar; now they are in flyouts and the bar has the room.
        let can_turn_pages = show_page_buttons(session_state.can_turn_pages());
        page_box_clone.set_visible(can_turn_pages);
        match &session_state {
            SessionState::NoSession => {
                app_label_clone.set_text("No session");
                time_display_clone.set_remaining(None);
                warning_for_timer.set_visible(false);
            }
            SessionState::Active {
                entry_name,
                started_at,
                time_limit_secs,
                ..
            } => {
                app_label_clone.set_text(entry_name);
                // Calculate remaining time based on elapsed time since session start
                let remaining = time_limit_secs.map(|limit| {
                    let elapsed = started_at.elapsed().as_secs();
                    limit.saturating_sub(elapsed)
                });
                time_display_clone.set_remaining(remaining);
                warning_for_timer.set_visible(false);
            }
            SessionState::Warning {
                entry_name,
                warning_issued_at,
                time_remaining_at_warning,
                message,
                severity,
                ..
            } => {
                app_label_clone.set_text(entry_name);
                // Calculate remaining time based on elapsed time since warning was issued
                let elapsed = warning_issued_at.elapsed().as_secs();
                let remaining = time_remaining_at_warning.saturating_sub(elapsed);
                time_display_clone.set_remaining(Some(remaining));
                // Use configuration-defined message if present, otherwise show time-based message
                let warning_text = message
                    .clone()
                    .unwrap_or_else(|| format!("Only {} seconds remaining!", remaining));
                warning_for_timer.set_text(&warning_text);

                // Apply severity-based CSS classes
                warning_for_timer.set_severity_class(Some(match severity {
                    lunchbox_api::WarningSeverity::Info => "warning-info",
                    lunchbox_api::WarningSeverity::Warn => "warning-warn",
                    lunchbox_api::WarningSeverity::Critical => "warning-critical",
                }));

                warning_for_timer.set_visible(true);
            }
            SessionState::Ending { reason, .. } => {
                app_label_clone.set_text("Session ending...");
                warning_for_timer.set_text(reason);
                warning_for_timer.set_visible(true);
            }
        }

        // Update network connectivity indicator. The HUD aggregates every
        // configured check: if any one is offline we surface the offline
        // icon so the bar matches the user-visible behavior (entries that
        // require internet are hidden as soon as any check fails).
        let internet = state.internet_status();
        if suspended {
            // The cached connectivity can't be trusted across a suspend; show
            // a neutral "checking" placeholder until the post-resume re-check
            // delivers a fresh StateChanged.
            network_box_clone.set_visible(true);
            network_box_clone.remove_css_class("network-online");
            network_box_clone.remove_css_class("network-offline");
            network_icon_clone.set_icon_name(Some("content-loading-symbolic"));
            network_box_clone.set_tooltip_text(Some("Checking connectivity…"));
        } else if internet.is_empty() {
            network_box_clone.set_visible(false);
        } else {
            network_box_clone.set_visible(true);
            let any_offline = internet.iter().any(|s| !s.available);
            network_box_clone.remove_css_class("network-online");
            network_box_clone.remove_css_class("network-offline");
            if any_offline {
                network_icon_clone.set_icon_name(Some("network-offline-symbolic"));
                network_box_clone.add_css_class("network-offline");
            } else {
                network_icon_clone.set_icon_name(Some("network-transmit-receive-symbolic"));
                network_box_clone.add_css_class("network-online");
            }
            let mut tooltip = String::from("Internet connectivity checks:");
            for status in &internet {
                tooltip.push('\n');
                tooltip.push_str(if status.available { "✓ " } else { "✗ " });
                tooltip.push_str(&status.target);
            }
            network_box_clone.set_tooltip_text(Some(&tooltip));
        }

        // The lock button appears with administrator mode and goes away with it.
        lock_button_clone.set_visible(admin_mode);

        // The taskbar, and with it the kiosk's own left-hand labels: in
        // administrator mode "No session" and a blank countdown say nothing,
        // and the space is wanted for the window list.
        taskbar_box_clone.set_visible(admin_mode);
        app_label_clone.set_visible(!admin_mode);
        time_display_clone.set_visible(!admin_mode);
        if admin_mode {
            rebuild_taskbar(&window_buttons_clone, &admin_windows(&state.windows()));
        }

        // Update the display-mode toggle and follow the active output (#87).
        // The button only appears while an external display is connected; its
        // tooltip names the action the toggle performs from the current mode.
        if let Some(ds) = state.display_state() {
            use lunchbox_api::DisplayMode;
            match (ds.has_secondary(), ds.mode) {
                (true, DisplayMode::Mirror) => {
                    display_button_clone.set_visible(true);
                    display_button_clone.set_tooltip_text(Some("Use external display only"));
                }
                (true, DisplayMode::ExternalOnly) => {
                    display_button_clone.set_visible(true);
                    display_button_clone.set_tooltip_text(Some("Mirror to external display"));
                }
                _ => display_button_clone.set_visible(false),
            }
            // The active output is the external in external-only mode, else the
            // primary. Re-anchor the layer-shell surface there so the HUD is
            // always visible on the screen the user is looking at.
            let active = match ds.mode {
                DisplayMode::ExternalOnly => ds.secondary.clone(),
                _ => ds.primary.clone(),
            };
            if let Some(name) = active
                && anchored_connector.borrow().as_deref() != Some(name.as_str())
                && let Some(monitor) = monitor_by_connector(&name)
            {
                // Force a clean unmap → remap onto the new output. When the
                // previously-anchored output is disabled (switching to
                // external-only), the compositor destroys the HUD's layer
                // surface but GTK still believes the window is visible — so
                // set_monitor/present alone won't recreate it and the HUD
                // vanishes. Hiding first resyncs GTK's mapped state, then
                // set_monitor + show builds a fresh surface on the live output.
                window_for_monitor.set_visible(false);
                window_for_monitor.set_monitor(&monitor);
                window_for_monitor.set_visible(true);
                *anchored_connector.borrow_mut() = Some(name);
            }
        }

        // Update battery
        if suspended {
            // Placeholder so a stale charge level isn't frozen on screen.
            battery_box_clone.set_visible(true);
            battery_icon_clone.set_icon_name(Some("battery-missing-symbolic"));
            battery_label_clone.set_text("--%");
        } else {
            let battery = BatteryStatus::read();
            let has_battery = battery.percent.is_some();
            battery_box_clone.set_visible(has_battery);
            if has_battery {
                battery_icon_clone.set_icon_name(Some(battery.icon_name()));
                if let Some(percent) = battery.percent {
                    battery_label_clone.set_text(&format!("{}%", percent));
                }
            }
        }

        // Update volume from cached state (updated via events, no polling needed).
        // Since `VolumeChanged` carries the whole snapshot, the slider bounds
        // below follow the active output's restrictions rather than whichever
        // ones happened to be in effect at connect time (issue #124).
        //
        // The flyout's widgets are read out of the cell rather than captured,
        // because a scale change replaces them; clone them out and drop the
        // borrow before touching them, the same discipline the prompts use.
        let (mute_toggle, mute_icon, volume_slider, volume_label) = {
            let control = volume_popover_for_timer.borrow();
            (
                control.toggle.clone(),
                control.toggle_icon.clone(),
                control.slider.clone(),
                control.label.clone(),
            )
        };
        if let Some(volume) = state.volume_info() {
            volume_icon_clone.set_icon_name(Some(volume.icon_name()));
            mute_icon.set_icon_name(Some(volume.icon_name()));
            volume_label.set_text(&format!("{}%", volume.percent));

            // Name the active output in the tooltip only. This is the
            // child-facing surface, so the bar itself stays uncluttered; device
            // names are long and mean nothing to the person using it.
            let tooltip = match volume.output.as_ref() {
                Some(o) if !o.description.is_empty() => {
                    format!("Volume \u{2014} {}", o.description)
                }
                _ => "Volume".to_string(),
            };
            volume_button_clone.set_tooltip_text(Some(&tooltip));

            // Only update slider if user is not actively dragging it
            if !slider_changing_for_update.get() {
                volume_slider.set_value(volume.percent as f64);
            }
            // Reset the changing flag after a short delay
            slider_changing_for_update.set(false);

            // Disable slider when muted or when restrictions don't allow changes
            let slider_enabled = !volume.muted && volume.restrictions.allow_change;
            volume_slider.set_sensitive(slider_enabled);
            // The restriction lands on the mute button itself now, not on the
            // bar icon: the icon opens the flyout, which is worth doing even
            // when muting is not allowed — the level may still be adjustable.
            mute_toggle.set_sensitive(volume.restrictions.allow_mute);
            // Safe to push unconditionally: the toggle's handler is on
            // `clicked`, which `set_active` does not emit.
            mute_toggle.set_active(volume.muted);

            // Update slider range based on restrictions
            let min = volume.restrictions.min_volume.unwrap_or(0) as f64;
            let max = volume.restrictions.max_volume.unwrap_or(100) as f64;
            volume_slider.set_range(min, max);
        } else {
            volume_label.set_text("--%");
            volume_slider.set_sensitive(false);
        }

        // Update brightness slider from cached state. Hidden entirely on
        // hosts that don't expose a backlight (`available=false`).
        let (auto_toggle, brightness_slider, brightness_label) = {
            let control = brightness_popover_for_timer.borrow();
            (
                control.toggle.clone(),
                control.slider.clone(),
                control.label.clone(),
            )
        };
        let backlight = state.brightness_info().filter(|b| b.available);
        brightness_button_clone.set_visible(backlight.is_some());
        if let Some(brightness) = backlight {
            brightness_icon_clone.set_icon_name(Some(brightness.icon_name()));
            brightness_label.set_text(&format!("{}%", brightness.percent));

            if !brightness_changing_for_update.get() {
                brightness_slider.set_value(brightness.percent as f64);
            }
            brightness_changing_for_update.set(false);

            brightness_slider.set_sensitive(brightness.restrictions.allow_change);

            let min = brightness.restrictions.min_brightness.unwrap_or(0) as f64;
            let max = brightness.restrictions.max_brightness.unwrap_or(100) as f64;
            brightness_slider.set_range(min, max);

            // Automatic is offered only where a light sensor exists; otherwise
            // the toggle is present but inert, so the flyout's shape does not
            // change from host to host.
            auto_toggle.set_sensitive(brightness.auto_available);
            // Same as mute: the handler is on `clicked`, so pushing the real
            // state in cannot echo an RPC back out. This is what retired the
            // `auto_updating` re-entrancy guard.
            auto_toggle.set_active(brightness.auto_enabled);
            // Keep the *bar* reporting what the toggle says, so the state is
            // still readable without opening the flyout. Automatic is the
            // expected state and renders plain; the icon lights up only when
            // the user has taken manual control, which is exactly what the
            // icon did back when it was the toggle.
            let manual = brightness.auto_available && !brightness.auto_enabled;
            if manual {
                brightness_button_clone.add_css_class("brightness-manual");
            } else {
                brightness_button_clone.remove_css_class("brightness-manual");
            }
        }

        glib::ControlFlow::Continue
    });

    HudContent {
        container,
        confirm_prompt,
        reset_prompt,
        slider_popovers: [volume_popover, brightness_popover],
        warning_popover: warning.popover.clone(),
    }
}

/// Find the GDK monitor whose connector name matches `connector` (e.g.
/// "eDP-1", "HDMI-A-1"), so the HUD can anchor its layer-shell surface to a
/// specific output (issue #87). Returns `None` if no monitor reports that
/// connector (e.g. it was just disabled).
fn monitor_by_connector(connector: &str) -> Option<gtk4::gdk::Monitor> {
    use gtk4::gio::prelude::ListModelExt;
    let monitors = gtk4::gdk::Display::default()?.monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i))
        .filter_map(|obj| obj.downcast::<gtk4::gdk::Monitor>().ok())
        .find(|m| m.connector().as_deref() == Some(connector))
}

/// Install an empty `CssProvider` at application priority and return it so
/// the caller can refresh its contents on the fly via `apply_scale`.
fn install_css_provider() -> gtk4::CssProvider {
    let provider = gtk4::CssProvider::new();
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("Could not get display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    provider
}

/// Horizontal padding of the confirm popover's surface, per side. Must match
/// the `padding` on `.confirm-close-popover > contents` in `CSS_TEMPLATE`,
/// which scales with the HUD factor.
const POPOVER_PADDING_PX: f64 = 14.0;

/// Spacing between the confirm prompt's message and its button row, at factor 1.0.
const CONFIRM_ROW_SPACING_PX: f64 = 12.0;

/// Spacing between the confirm prompt's two buttons, at factor 1.0.
const CONFIRM_BUTTON_SPACING_PX: f64 = 8.0;

/// Spacing between a pop-out control's toggle, slider and readout, at factor 1.0.
const SLIDER_POPOVER_SPACING_PX: f64 = 8.0;

/// Which of the two pop-out controls a [`SliderPopover`] is.
///
/// The two are the same shape — a toggle, a slider, a percentage — and differ
/// only in what they are called and which RPCs they drive, so one builder
/// makes both and this enum carries the differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SliderControl {
    Volume,
    Brightness,
}

impl SliderControl {
    /// The `.volume-slider` / `.brightness-slider` styling, which is what
    /// gives each control its own highlight colour.
    fn slider_class(self) -> &'static str {
        match self {
            Self::Volume => "volume-slider",
            Self::Brightness => "brightness-slider",
        }
    }

    fn label_class(self) -> &'static str {
        match self {
            Self::Volume => "volume-label",
            Self::Brightness => "brightness-label",
        }
    }

    /// The toggle that sits at the head of the flyout: mute for volume,
    /// automatic for brightness. Both were in the bar before the flyout
    /// existed — the bar's icon *was* the toggle — and both move in here,
    /// because the icon now has a job of its own.
    fn toggle_class(self) -> &'static str {
        match self {
            Self::Volume => "mute-toggle",
            Self::Brightness => "brightness-toggle",
        }
    }

    fn toggle_icon(self) -> &'static str {
        match self {
            Self::Volume => "audio-volume-medium-symbolic",
            Self::Brightness => "display-brightness-symbolic",
        }
    }

    fn toggle_tooltip(self) -> &'static str {
        match self {
            Self::Volume => "Mute",
            Self::Brightness => "Automatic brightness",
        }
    }
}

/// One of the bar's pop-out controls: the flyout that opens from the volume or
/// brightness icon (issue #178).
///
/// Both sliders used to sit in the bar beside their icons. A pair of them is
/// ~200 logical pixels of a 1280px bar, and over a third of the *minimum
/// height* of the vertical one — which is what left a reading session on a
/// short screen with nowhere to put the page-turn buttons, and what the #160
/// shortening was trying and failing to buy back. Opening them from the icon
/// costs the bar a 32px button instead, gives the slider more room than it
/// ever had inline, and is how a tray volume control behaves on every desktop,
/// so it needs no explaining to the person using it.
///
/// **Rebuilt on every `HudScaleChanged`, like the confirm prompts.** It lives
/// hidden across a scale change and [`align_popover_to_button`] measures it
/// just before showing it, which is precisely the case issue #118 says a fresh
/// widget is needed for: GTK leaves a hidden widget's style alone, so one that
/// survived the change would be measured at the previous factor's size. The
/// 500ms timer re-pushes value, range and sensitivity every tick, so a rebuilt
/// control has the live state back long before anyone can open it.
struct SliderPopover {
    popover: gtk4::Popover,
    /// The child box, measured by [`align_popover_to_button`]. A `GtkPopover`
    /// is a native surface and reports a near-zero size before it is mapped.
    content: gtk4::Box,
    /// Mute, or automatic brightness.
    toggle: gtk4::ToggleButton,
    toggle_icon: gtk4::Image,
    slider: gtk4::Scale,
    label: gtk4::Label,
}

/// Build one pop-out control's widgets, parented to the bar icon that opens it
/// and sized for HUD scale `factor`.
///
/// Layout only — the callers below connect the handlers, because that is the
/// whole of what differs between volume and brightness. The row is horizontal
/// in **both** bar layouts: a popover is not the bar, so it does not inherit
/// the bar's axis, and a horizontal slider is what the flyout has room for
/// whichever edge it flew out of.
fn build_slider_popover(
    control: SliderControl,
    anchor: &gtk4::Button,
    window: &gtk4::ApplicationWindow,
    factor: f64,
    orientation: HudOrientation,
) -> SliderPopover {
    let popover = gtk4::Popover::new();
    popover.set_parent(anchor);
    popover.add_css_class("slider-popover");
    // Out of the bar and into the screen, the same direction and for the same
    // reason as the confirm prompts: a layer-shell popup that lands past the
    // screen edge is clipped rather than slid back on.
    popover.set_position(if orientation.is_vertical() {
        gtk4::PositionType::Right
    } else {
        gtk4::PositionType::Bottom
    });
    // Autohide, so tapping the activity puts the flyout away. As with the
    // confirm prompts that needs an input grab, which needs the layer surface
    // to accept keyboard focus — hence the `OnDemand` switch at the press and
    // the `closed` handler below that gives it straight back. The always-
    // present bar must never hold keyboard focus itself.
    popover.set_autohide(true);

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing((SLIDER_POPOVER_SPACING_PX * factor).round() as i32)
        .build();

    let toggle_icon = gtk4::Image::from_icon_name(control.toggle_icon());
    toggle_icon.set_pixel_size((f64::from(BASE_ICON_PIXEL_SIZE) * factor).round() as i32);
    let toggle = gtk4::ToggleButton::builder()
        .child(&toggle_icon)
        .has_frame(false)
        .tooltip_text(control.toggle_tooltip())
        .build();
    toggle.add_css_class("indicator-button");
    toggle.add_css_class(control.toggle_class());
    content.append(&toggle);

    let slider = gtk4::Scale::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .draw_value(false)
        .build();
    slider.add_css_class(control.slider_class());
    slider.set_width_request((f64::from(BASE_SLIDER_LENGTH) * factor).round() as i32);
    content.append(&slider);

    // The percentage comes back for both layouts. It was dropped from the
    // vertical bar because "100%" does not fit across 48px, and from a reading
    // session because the bar was full; the flyout has room for it in every
    // case, so neither exception survives.
    let label = gtk4::Label::new(Some("--%"));
    label.add_css_class(control.label_class());
    label.set_width_chars(4);
    content.append(&label);

    popover.set_child(Some(&content));

    let window_for_closed = window.clone();
    popover.connect_closed(move |_| {
        window_for_closed.set_keyboard_mode(KeyboardMode::None);
    });

    SliderPopover {
        popover,
        content,
        toggle,
        toggle_icon,
        slider,
        label,
    }
}

/// The volume flyout: mute, level, percentage.
fn build_volume_popover(
    anchor: &gtk4::Button,
    window: &gtk4::ApplicationWindow,
    factor: f64,
    orientation: HudOrientation,
    requests: &mpsc::Sender<u8>,
    dragging: &std::rc::Rc<std::cell::Cell<bool>>,
) -> SliderPopover {
    let control = build_slider_popover(SliderControl::Volume, anchor, window, factor, orientation);
    control.slider.set_range(0.0, 100.0);
    control.slider.set_increments(5.0, 10.0);

    // `clicked` rather than `toggled`: a `GtkToggleButton` emits `toggled` from
    // `set_active` too, so the update loop pushing the real mute state back
    // into the button would echo an RPC on every tick. `clicked` is only ever
    // the user. That is what retired the `auto_updating` re-entrancy guard the
    // brightness toggle used to carry, and it was measured rather than assumed:
    // flapping `set_active` from the timer for 15s produced 20 `toggled` and
    // **0** `clicked`.
    control.toggle.connect_clicked(|_| {
        if let Err(e) = crate::volume::toggle_mute() {
            tracing::error!("Failed to toggle mute: {}", e);
        }
    });

    let tx = requests.clone();
    let dragging = dragging.clone();
    control
        .slider
        .connect_change_value(move |slider, _, value| {
            dragging.set(true);
            let _ = tx.send(value.clamp(0.0, 100.0) as u8);
            // Move the knob straight away; the debounced worker catches up.
            slider.set_value(value);
            glib::Propagation::Stop
        });

    control
}

/// The brightness flyout: automatic on/off, level, percentage.
fn build_brightness_popover(
    anchor: &gtk4::Button,
    window: &gtk4::ApplicationWindow,
    factor: f64,
    orientation: HudOrientation,
    requests: &mpsc::Sender<u8>,
    dragging: &std::rc::Rc<std::cell::Cell<bool>>,
) -> SliderPopover {
    let control = build_slider_popover(
        SliderControl::Brightness,
        anchor,
        window,
        factor,
        orientation,
    );
    control.slider.set_range(0.0, 100.0);
    control.slider.set_increments(5.0, 10.0);

    // See the note on the mute toggle: `clicked` is the user's press only, so
    // the update loop can set the button's state without echoing an RPC back.
    control.toggle.connect_clicked(|btn| {
        if let Err(e) = crate::brightness::set_auto_brightness(btn.is_active()) {
            tracing::error!("Failed to set auto brightness: {}", e);
        }
    });

    let tx = requests.clone();
    let dragging = dragging.clone();
    control
        .slider
        .connect_change_value(move |slider, _, value| {
            dragging.set(true);
            let _ = tx.send(value.clamp(0.0, 100.0) as u8);
            slider.set_value(value);
            glib::Propagation::Stop
        });

    control
}

/// Open a pop-out control, aligned so it cannot land past the end of the bar.
///
/// Takes its own clones and drops the borrow before `popup()`, the same
/// discipline the confirm prompts use: popping up runs signal handlers, and
/// one of them reaching back into the cell would panic.
fn open_slider_popover(
    control: &std::rc::Rc<std::cell::RefCell<SliderPopover>>,
    button: &gtk4::Button,
    window: &gtk4::ApplicationWindow,
    factor: f64,
    orientation: HudOrientation,
) {
    let (popover, content) = {
        let control = control.borrow();
        (control.popover.clone(), control.content.clone())
    };
    window.set_keyboard_mode(KeyboardMode::OnDemand);
    align_popover_to_button(&popover, &content, button, factor, orientation);
    popover.popup();
}

/// The HUD's close-confirmation prompt: the popover itself, the content box
/// (measured to right-align it against the "X"), and the message label (retitled
/// with the activity's name each time it is shown).
struct ConfirmPrompt {
    popover: gtk4::Popover,
    content: gtk4::Box,
    label: gtk4::Label,
}

/// Build the "really end?" prompt (issue #78), parented to the "X" button and
/// sized for HUD scale `factor`.
///
/// **Rebuilt from scratch on every HudScaleChanged rather than restyled in
/// place.** GTK validates a widget's style while it is mapped and leaves it
/// alone otherwise, so a popover hidden across a scale change keeps the previous
/// factor's style: it measures — and can paint — at the old size while the
/// always-mapped bar around it is already correct, which is what "only the
/// dialog renders un-counter-scaled" looks like (issue #118). Re-rooting the
/// contents does not clear it; freshly built widgets, on the other hand, have no
/// cached style and take the current stylesheet immediately (verified in the
/// headless harness: the same label measures 183px at factor 1.0 and 366px at
/// 2.0 when newly built while hidden, against a stale 218px for the widget that
/// survived the change).
///
/// Everything sized here in *widget properties* rather than CSS — the two box
/// spacings — is multiplied by `factor` for the same reason the timer rescales
/// the bar's spacings: `scale_px_literals` only reaches the stylesheet.
fn build_confirm_prompt(
    action_button: &gtk4::Button,
    window: &gtk4::ApplicationWindow,
    factor: f64,
    action: ConfirmAction,
    orientation: HudOrientation,
) -> ConfirmPrompt {
    // Parented to the button, so on the layer-shell overlay it renders as a
    // child popup above the running activity.
    let popover = gtk4::Popover::new();
    popover.set_parent(action_button);
    popover.add_css_class("confirm-close-popover");
    // Drop the prompt straight down from the "X" button. The button sits at the
    // extreme right of the bar, so the default (horizontally centered) placement
    // would put half the popover past the right screen edge — and neither GTK
    // nor the compositor slides an oversized layer-shell popup back on-screen,
    // so it gets clipped (issue #97). `align_popover_to_button` additionally
    // offsets it left to keep it fully visible; Bottom gives it unlimited
    // vertical room.
    //
    // The vertical bar puts the same buttons down the left edge instead, so
    // the prompt drops to the *right*, into the screen, for exactly the same
    // reason: that is the direction with room. `align_popover_to_button` then
    // does the along-the-bar nudge on whichever axis the bar runs.
    popover.set_position(if orientation.is_vertical() {
        gtk4::PositionType::Right
    } else {
        gtk4::PositionType::Bottom
    });
    // Autohide so the prompt dismisses itself when it loses focus (the user
    // taps the activity, presses Escape, etc.). Autohide relies on an input
    // grab that needs the layer surface to accept keyboard focus, so we switch
    // the HUD to on-demand keyboard interactivity only while the prompt is up
    // (see the click and `closed` handlers) and back to none otherwise, so the
    // always-present bar never steals keyboard focus from the activity.
    popover.set_autohide(true);

    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing((CONFIRM_ROW_SPACING_PX * factor).round() as i32)
        .build();
    let label = gtk4::Label::new(Some("End this activity?"));
    label.add_css_class("confirm-close-message");
    label.set_wrap(true);
    label.set_max_width_chars(28);
    content.append(&label);

    let button_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing((CONFIRM_BUTTON_SPACING_PX * factor).round() as i32)
        .homogeneous(true)
        .build();
    let cancel_button = gtk4::Button::with_label("Cancel");
    let end_button = gtk4::Button::with_label(action.button_label());
    end_button.add_css_class("destructive-action");
    button_row.append(&cancel_button);
    button_row.append(&end_button);
    content.append(&button_row);
    popover.set_child(Some(&content));

    let popover_for_cancel = popover.clone();
    cancel_button.connect_clicked(move |_| {
        popover_for_cancel.popdown();
    });

    let popover_for_end = popover.clone();
    end_button.connect_clicked(move |_| {
        popover_for_end.popdown();
        action.run(default_socket_path());
    });

    // Release the on-demand keyboard grab whenever the prompt goes away, no
    // matter how it was dismissed (Cancel, End, Escape, focus loss, or a
    // programmatic popdown when the activity ends by other means), so the HUD
    // returns to not competing for keyboard focus.
    let window_for_closed = window.clone();
    popover.connect_closed(move |_| {
        window_for_closed.set_keyboard_mode(KeyboardMode::None);
    });

    ConfirmPrompt {
        popover,
        content,
        label,
    }
}

/// Right-align `popover` to `button` instead of letting GTK center it.
///
/// A `Bottom` popover is centered on its parent, and the "X" sits at the extreme
/// right of the bar, so half of a centered prompt lands past the right edge of
/// the output — and neither GTK nor sway slides an oversized layer-shell popup
/// back on-screen, so it is simply clipped (issue #97). Shifting the center left
/// by (popover_width - button_width)/2 lands the popover's right edge on the
/// button's right edge, fully on-screen, without relying on any slide-to-fit.
///
/// The pop-out volume and brightness controls (issue #178) use it too. Their
/// icons sit further in from the end of the bar, so how much room they have
/// depends on which indicators the host shows — a box with a backlight but no
/// battery leaves the brightness icon close enough to the end for a centered
/// flyout to overhang it. Aligning to the bar's end unconditionally is both
/// safe in every combination and what a tray flyout does anyway.
///
/// `content` is the popover's child box: a `GtkPopover` is a native surface and
/// reports a near-zero preferred size before it is mapped, so the width has to
/// come from a plain widget inside it, plus the popover's own chrome (the
/// `> contents` padding, which follows the HUD scale `factor`).
///
///
/// The measurement is trustworthy because the prompt is rebuilt on every scale
/// change (see `build_confirm_prompt`): its widgets are always styled for the
/// factor in force, so this never reads the previous factor's layout.
fn align_popover_to_button(
    popover: &gtk4::Popover,
    content: &gtk4::Box,
    button: &gtk4::Button,
    factor: f64,
    orientation: HudOrientation,
) {
    // The clipping is always along the bar, so the measurement is taken on the
    // bar's own axis: a horizontal bar runs out of room to the right of the
    // "X", a vertical one runs out above it.
    let axis = orientation.flow();
    let (_, content_len, _, _) = content.measure(axis, -1);
    let chrome = (2.0 * POPOVER_PADDING_PX * factor).round() as i32;
    let popover_len = content_len + chrome;
    let (_, button_len, _, _) = button.measure(axis, -1);
    let offset = (button_len - popover_len) / 2;
    tracing::debug!(
        factor,
        ?orientation,
        content_len,
        chrome,
        button_len,
        offset,
        "Aligning confirm popover"
    );
    // A vertical bar anchors the button at the *top*, so the overhang to pull
    // back is below the button rather than beside it -- the same shift, on the
    // other axis.
    if orientation.is_vertical() {
        popover.set_offset(0, -offset);
    } else {
        popover.set_offset(offset, 0);
    }
}

/// Apply the current scale factor to the HUD: regenerate the stylesheet
/// with px values multiplied by `factor`, and resize the window so its
/// physical thickness stays consistent with the pre-scale value. Called once
/// on construction and again every time lunchboxd sends a HudScaleChanged.
///
/// "Thickness" is the bar's short axis, and which axis that is depends on
/// `orientation`: a vertical bar reserves its exclusive zone horizontally, so
/// it has to request a default *width* — requesting a height would let the
/// surface size itself to its content and give the compositor nothing to
/// reserve.
fn apply_scale(
    provider: &gtk4::CssProvider,
    window: &gtk4::ApplicationWindow,
    base_thickness: i32,
    factor: f64,
    orientation: HudOrientation,
) {
    let scaled = ((base_thickness as f64) * factor).round() as i32;
    tracing::info!(
        factor,
        thickness = scaled,
        ?orientation,
        "Applying HUD scale"
    );
    if orientation.is_vertical() {
        window.set_default_width(scaled);
    } else {
        window.set_default_height(scaled);
    }
    window.set_exclusive_zone(scaled);
    provider.load_from_data(&css_for_scale(factor));
}

/// Build the HUD stylesheet with `factor`-scaled px values. Every `Npx`
/// literal in `CSS_TEMPLATE` is multiplied by `factor` so the layer-shell
/// surface stays a constant physical size when lunchboxd drops the
/// compositor scale to 1.0 for an XWayland activity (see the
/// HudScaleChanged event in lunchbox-api). Non-px numbers (timings,
/// opacities, rgba components) are passed through unchanged.
fn css_for_scale(factor: f64) -> String {
    scale_px_literals(CSS_TEMPLATE, factor)
}

fn scale_px_literals(template: &str, factor: f64) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len() + 64);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let num_str = &template[start..i];
            if i + 1 < bytes.len() && &bytes[i..i + 2] == b"px" {
                let n: f64 = num_str.parse().unwrap_or(0.0);
                out.push_str(&((n * factor).round() as i32).to_string());
                out.push_str("px");
                i += 2;
            } else {
                out.push_str(num_str);
            }
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

const CSS_TEMPLATE: &str = r#"
        :root {
            --hud-bg: rgba(30, 30, 30, 0.95);
            --text-primary: white;
            --text-secondary: #d8dee9;
            --color-info: #88c0d0;
            --color-warning: #ebcb8b;
            --color-critical: #ff6b6b;
            --color-success: #a3be8c;
            --hover-bg: rgba(255, 255, 255, 0.1);
        }

        /* Base font size for the whole bar. Every size that should follow the
           HUD scale factor has to be written *here*, in px, for
           `scale_px_literals` to counter-scale it; anything left to the GTK
           theme keeps its logical-pixel value and so shrinks on screen by
           1/factor once sway drops to scale 1.0. Setting the size on the root
           means a label that doesn't name its own font-size inherits a scaled
           one instead of falling back to the theme's default (issue #114). */
        .hud-bar {
            background-color: var(--hud-bg);
            border: none;
            margin: 0;
            padding: 6px 12px;
            font-size: 14px;
        }

        .app-name {
            font-weight: bold;
            font-size: 14px;
            color: var(--text-primary);
        }

        /* The vertical bar (issue #171). Everything above applies to it
           unchanged; these are the handful of rules that cannot be the same
           when the long axis is the other one.

           Each still has to be written in px here for `scale_px_literals` to
           counter-scale it -- the vertical layout gets no exemption from the
           rule at the top of this stylesheet. */
        .hud-bar.hud-vertical {
            /* The bar's own padding, turned with it: the 6px that used to be
               above and below the row is now beside the column. */
            padding: 12px 6px;
        }

        /* The sliders used to need an axis swap here: written for a
           horizontal bar they name 80px of length and a 4px-thick trough, and
           left alone on a vertical bar they demanded that length *across* it
           -- the surface measured 124px wide instead of 48px. They no longer
           need one, because they are no longer in the bar: both open out of
           their icon as a flyout, which is a popover and so keeps its own
           horizontal axis whichever edge the bar is on (issue #178).

           The lesson the swap taught is still worth keeping for whatever comes
           next, and it has its own trap: state the axis, never the *length*. A
           CSS minimum is a floor GTK takes the maximum of against the widget's
           size request, so a `min-height` restated here silently outranks the
           request -- which is how the #160 reading-session shortening came to
           be inert on the vertical bar, and how #178 ran out of height and
           clipped the page-turn buttons off the bottom. */

        .hud-vertical .network-indicator {
            padding: 2px 0;
        }

        /* The one readout that still has to fit *across* a 48px bar rather
           than along it. At the bar's 14px "100%" is wider than the space
           between the paddings, so it gets a size that fits. The volume and
           brightness percentages used to be dropped from this bar for the same
           reason; they are in the flyouts now, which have room (issue #178). */
        .hud-vertical .battery-label {
            font-size: 11px;
        }

        /* The message that no longer fits in the bar. It is a popover rather
           than part of the bar (see `WarningBanner`), so it needs the bar's
           own background -- a popover does not inherit it -- and a width bound
           so an operator's long sentence wraps instead of running off the
           screen. */
        .warning-popover > contents {
            background-color: var(--hud-bg);
            border: none;
            border-radius: 4px;
            padding: 8px 12px;
        }

        .warning-popover .warning-text {
            font-size: 14px;
        }

        .time-display {
            font-family: monospace;
            font-size: 14px;
            color: var(--color-info);
        }

        .time-display.time-warning {
            color: var(--color-warning);
        }

        .time-display.time-critical {
            color: var(--color-critical);
            animation: blink 1s infinite;
        }

        @keyframes blink {
            50% { opacity: 0.5; }
        }

        .warning-banner {
            background-color: rgba(235, 203, 139, 0.2);
            border-radius: 4px;
            padding: 4px 12px;
        }

        .warning-banner.warning-info {
            background-color: rgba(136, 192, 208, 0.2);
        }

        .warning-banner.warning-info .warning-text {
            color: var(--color-info);
        }

        .warning-banner.warning-warn {
            background-color: rgba(235, 203, 139, 0.2);
        }

        .warning-banner.warning-warn .warning-text {
            color: var(--color-warning);
        }

        .warning-banner.warning-critical {
            background-color: rgba(255, 107, 107, 0.2);
            animation: blink 1s infinite;
        }

        .warning-banner.warning-critical .warning-text {
            color: var(--color-critical);
        }

        .warning-text {
            color: var(--color-warning);
            font-weight: bold;
        }

        image {
            color: var(--text-primary);
        }

        /* The analog clock draws itself in whatever colour CSS resolves for
           it (`Widget::color`), and a `GtkDrawingArea` is not an `image` node,
           so without this it inherits the *theme's* default text colour --
           near-black, and all but invisible against the bar. */
        .analog-clock {
            color: var(--text-primary);
        }

        .indicator-button,
        .control-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: 4px;
            color: var(--text-primary);
        }

        /* The page-turn buttons exist for a finger, on a panel with no
           keyboard, so they get a wider touch target than the 32px an
           indicator gets — a mis-tap here turns no page and reads as the
           activity being broken. Width only: the bar's height is its
           layer-shell exclusive zone, and a taller child pushes the window
           past it, so the HUD grows over the activity while a book is open. */
        .page-button {
            min-width: 44px;
        }

        .indicator-button:hover,
        .control-button:hover {
            background-color: var(--hover-bg);
        }

        /* Administrator mode's taskbar (issue #154). Window buttons carry a
           label rather than an icon, so they need room to the sides that the
           square indicator buttons do not. */
        .indicator-button label {
            padding: 0 6px;
        }

        /* Which window the keyboard is talking to. The taskbar is the only
           place that says so — the kiosk hides every border and title bar. */
        .taskbar-focused {
            background-color: var(--hover-bg);
        }

        /* Automatic brightness is the default, so its toggle stays plain when
           checked (auto on) and lights up only in the *manual* state
           (unchecked, and only when a sensor makes auto an option at all),
           using the brightness bar's own highlight colour so the two read as
           one control.

           `.brightness-manual` is the same state shown on the *bar* icon,
           which the update loop sets. The toggle itself moved into the flyout
           with issue #178, and without this the bar would have stopped saying
           who is driving the backlight until someone opened the flyout. */
        .brightness-toggle:not(:checked):not(:disabled),
        .indicator-button.brightness-manual {
            background-color: var(--color-warning);
        }

        .brightness-toggle:not(:checked):not(:disabled) image,
        .indicator-button.brightness-manual image {
            color: #2e3440;
        }

        /* A muted volume is worth showing on its own toggle the same way, so
           the flyout says which state it is in rather than only the icon
           shape. Checked means muted here, the opposite of the brightness
           toggle above, because muted is the exceptional state. */
        .mute-toggle:checked:not(:disabled) {
            background-color: var(--color-critical);
        }

        .mute-toggle:checked:not(:disabled) image {
            color: #2e3440;
        }

        /* The GTK theme shades a *checked* toggle button by default. Automatic
           brightness (checked) must look completely plain, so clear that
           shading — keeping only the normal hover feedback. */
        .brightness-toggle:checked {
            background-color: transparent;
            background-image: none;
            box-shadow: none;
        }

        .brightness-toggle:checked:hover {
            background-color: var(--hover-bg);
        }

        .close-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: 4px;
            color: var(--color-critical);
        }

        .close-button:hover {
            background-color: rgba(191, 97, 106, 0.3);
        }

        .battery-label {
            font-size: 12px;
            color: var(--text-primary);
        }

        .network-indicator {
            padding: 0 2px;
        }

        .network-indicator.network-online image {
            color: var(--color-success);
        }

        .network-indicator.network-offline image {
            color: var(--color-critical);
        }

        /* Length comes from the widget's size request (`BASE_SLIDER_LENGTH`),
           which is what lets it follow the HUD scale factor. Stating a floor
           here as well would outrank a shorter request -- see the note by the
           `.hud-vertical` rules above. */
        .volume-slider {
            min-width: 0px;
        }

        .volume-slider trough {
            min-height: 4px;
            border-radius: 2px;
            background-color: rgba(255, 255, 255, 0.2);
        }

        .volume-slider highlight {
            min-height: 4px;
            border-radius: 2px;
            background-color: var(--color-info);
        }

        /* 16px and the -8px overhang are what the GTK theme gives the slider
           node on its own, so at factor 1.0 these change nothing — but stating
           them here is what lets the knob grow with the rest of the HUD under
           the counter-scale. The old 12px was below the theme's own minimum, so
           the theme won at factor 1.0 and the knob ended up *smaller* than
           normal at 1.5, making it hard to hit on a touchscreen (issue #114).
           The negative margin has to be restated for the same reason: it is
           what keeps the knob overhanging the trough by a constant amount, and
           it also decides how much of the knob the trough has to accommodate
           (an unscaled -8px against a scaled knob thickens the bar). */
        .volume-slider slider {
            min-width: 16px;
            min-height: 16px;
            margin: -8px;
            border-radius: 50%;
            background-color: var(--text-primary);
        }

        .volume-slider:disabled trough {
            background-color: rgba(255, 255, 255, 0.1);
        }

        .volume-slider:disabled highlight {
            background-color: rgba(136, 192, 208, 0.5);
        }

        .volume-label {
            font-size: 12px;
            color: var(--text-secondary);
            min-width: 3em;
            text-align: right;
        }

        /* Matches `.volume-slider` -- see the note there. */
        .brightness-slider {
            min-width: 0px;
        }

        .brightness-slider trough {
            min-height: 4px;
            border-radius: 2px;
            background-color: rgba(255, 255, 255, 0.2);
        }

        .brightness-slider highlight {
            min-height: 4px;
            border-radius: 2px;
            background-color: var(--color-warning);
        }

        /* Matches `.volume-slider slider` — see the note there. */
        .brightness-slider slider {
            min-width: 16px;
            min-height: 16px;
            margin: -8px;
            border-radius: 50%;
            background-color: var(--text-primary);
        }

        .brightness-slider:disabled trough {
            background-color: rgba(255, 255, 255, 0.1);
        }

        .brightness-slider:disabled highlight {
            background-color: rgba(235, 203, 139, 0.5);
        }

        .brightness-label {
            font-size: 12px;
            color: var(--text-secondary);
            min-width: 3em;
            text-align: right;
        }

        .clock-label {
            font-family: monospace;
            font-size: 14px;
            color: var(--text-primary);
        }

        .mock-time-indicator {
            font-size: 10px;
            font-weight: bold;
            color: var(--color-warning);
            margin-left: 4px;
        }

        /* Opaque dark surface with explicit colors (not theme variables) so
           the prompt keeps strong text contrast regardless of the system GTK
           theme and never lets the bright activity behind it bleed through.
           The arrow (the triangle pointing at the "X") is a separate CSS node
           and must be recolored to match the box. */
        .confirm-close-popover > contents {
            background-color: #1e1e1e;
            border-radius: 8px;
            padding: 14px;
            /* The popover is its own surface, so state the base font size here
               too rather than relying on inheriting the bar's (issue #114):
               without it the Cancel / End labels keep the theme's unscaled
               size while the box around them grows. */
            font-size: 14px;
        }

        /* The pop-out volume / brightness controls (issue #178). Same opaque
           surface as the prompt above and for the same reasons: a popover does
           not inherit the bar's background, the activity behind it must not
           bleed through, and the base font size has to be stated here or the
           readout inside falls back to the theme's unscaled default (#114). */
        .slider-popover > contents {
            background-color: #1e1e1e;
            border-radius: 8px;
            padding: 14px;
            font-size: 14px;
        }

        .slider-popover > arrow {
            background-color: #1e1e1e;
            border: none;
        }

        .confirm-close-popover > arrow {
            background-color: #1e1e1e;
            border: none;
        }

        .confirm-close-message {
            color: #ffffff;
            font-size: 15px;
            font-weight: bold;
        }

        /* Theme buttons paint a gradient via background-image, which a bare
           background-color won't override, so clear it and set explicit
           high-contrast fills: a light Cancel with dark text, a red End with
           white text. */
        .confirm-close-popover button {
            min-height: 32px;
            padding: 6px 14px;
            border-radius: 4px;
            border: none;
            background-image: none;
            color: #2e3440;
            background-color: #d8dee9;
            /* State the font-size on the button node itself, not just on
               `> contents`. #114 set the base size on the popover surface
               expecting the Cancel / End labels to inherit it, but the GTK
               theme sets an explicit `font-size` on `button`, which is more
               specific than the inherited `> contents` value and wins the
               cascade — so the labels kept the theme's logical-pixel size and
               rendered 1/factor too small under the counter-scale, while the
               button box around them (min-height/padding, stated here in px)
               grew. Restating it here, at higher specificity than the theme's
               bare `button`, is what lets the label follow the HUD factor. */
            font-size: 14px;
        }

        .confirm-close-popover button:hover {
            background-color: #e5e9f0;
        }

        .confirm-close-popover button.destructive-action {
            color: #ffffff;
            background-color: #bf616a;
        }

        .confirm-close-popover button.destructive-action:hover {
            background-color: #d08770;
        }
    "#;

/// Keep the taskbar's window list current while administrator mode is on.
///
/// Polled rather than pushed: nothing broadcasts a window opening or closing —
/// `EventPayload` has no variant for it, the same gap the companion app's
/// window screen works around — so this is the only way to see one appear.
///
/// Idle outside the mode. In the kiosk there is nothing to switch between, so
/// the taskbar is hidden and a poll would be pure cost on a handheld; the loop
/// wakes twice a second, checks a bool, and goes back to sleep.
fn run_window_poll(socket_path: PathBuf, state: SharedState) -> anyhow::Result<()> {
    let rt = Runtime::new()?;
    rt.block_on(async {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if !state.admin_mode() {
                // Drop a stale list, so re-entering the mode never shows
                // windows that closed while nobody was looking.
                if !state.windows().is_empty() {
                    state.set_windows(Vec::new());
                }
                continue;
            }
            match IpcClient::connect(&socket_path).await {
                Ok(mut client) => match client.list_windows().await {
                    Ok(windows) => state.set_windows(windows),
                    // Keep the previous list rather than blanking the taskbar:
                    // the ids under those buttons are what a click acts on, and
                    // pulling them out from under a finger is worse than a list
                    // that is half a second stale.
                    Err(e) => tracing::debug!(error = %e, "list_windows failed"),
                },
                Err(e) => tracing::debug!(error = %e, "could not connect for the window poll"),
            }
        }
    })
}

fn run_event_loop(socket_path: PathBuf, state: SharedState) -> anyhow::Result<()> {
    let rt = Runtime::new()?;

    rt.block_on(async {
        loop {
            tracing::info!("Connecting to lunchboxd at {:?}", socket_path);

            match IpcClient::connect(&socket_path).await {
                Ok(mut client) => {
                    tracing::info!("Connected to lunchboxd");

                    // Get initial volume before subscribing (can't send RPCs after subscribe)
                    match client.get_volume().await {
                        Ok(info) => {
                            tracing::debug!("Got initial volume: {}%", info.percent);
                            state.set_initial_volume(info);
                        }
                        Err(e) => tracing::warn!("Failed to get initial volume: {}", e),
                    }

                    // Same for brightness. Returns available=false when the
                    // host has no backlight, which is the signal to the UI
                    // that it should hide the slider entirely.
                    match client.get_brightness().await {
                        Ok(info) => {
                            tracing::debug!(
                                "Got initial brightness: {}% (available={})",
                                info.percent,
                                info.available,
                            );
                            state.set_initial_brightness(info);
                        }
                        Err(e) => tracing::warn!("Failed to get initial brightness: {}", e),
                    }

                    // Seed the counter-scale factor. lunchboxd only *broadcasts*
                    // HudScaleChanged when it changes — at launch and at exit of
                    // an `xwayland_native_resolution` activity — so a HUD that
                    // was not subscribed at that instant (started late, or its
                    // connection dropped and reconnected mid-activity) would
                    // render every element, and the close-confirmation prompt
                    // most visibly, 1/factor too small for the rest of the
                    // session, with nothing to correct it (issue #118). Asking
                    // on every connect makes that self-healing.
                    match client.get_hud_scale().await {
                        Ok(factor) => {
                            tracing::debug!(factor, "Seeded HUD scale factor");
                            state.handle_event(&lunchbox_api::Event::new(
                                lunchbox_api::EventPayload::HudScaleChanged { factor },
                            ));
                        }
                        Err(e) => tracing::warn!("Failed to get initial HUD scale: {}", e),
                    }

                    // Seed the screen edge for exactly the same reason
                    // (issue #171). `HudOrientationChanged` fires only when
                    // the effective edge moves — when an activity with its own
                    // `hud_orientation` starts, and again when it ends — so a
                    // HUD that connected in between would lay itself out on
                    // the wrong edge, with its exclusive zone reserved on the
                    // wrong side of the activity, for the rest of the session.
                    match client.get_hud_orientation().await {
                        Ok(orientation) => {
                            tracing::debug!(?orientation, "Seeded HUD orientation");
                            state.set_orientation(orientation);
                        }
                        Err(e) => tracing::warn!("Failed to get initial HUD orientation: {}", e),
                    }

                    // Seed the display arrangement so the mirror/external toggle
                    // and active-output anchor are correct before any hotplug
                    // event fires (issue #87).
                    match client.get_display_state().await {
                        Ok(ds) => state.set_display_state(ds),
                        Err(e) => tracing::warn!("Failed to get initial display state: {}", e),
                    }

                    // Pull a fresh service snapshot so the network indicator
                    // (and any other state-derived UI) is populated even when
                    // no event has fired since the HUD connected.
                    match client.service_state().await {
                        Ok(snapshot) => {
                            state.handle_event(&lunchbox_api::Event::new(
                                lunchbox_api::EventPayload::StateChanged(snapshot),
                            ));
                        }
                        Err(e) => tracing::warn!("Failed to get initial state: {}", e),
                    }

                    let mut stream = match client.subscribe().await {
                        Ok(stream) => stream,
                        Err(e) => {
                            tracing::error!("Failed to subscribe: {}", e);
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    };

                    loop {
                        match stream.next().await {
                            Ok(event) => {
                                tracing::debug!("Received event: {:?}", event);
                                state.handle_event(&event);
                            }
                            Err(e) => {
                                tracing::error!("Event stream error: {}", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to lunchboxd: {}", e);
                }
            }

            // Wait before reconnecting
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(id: u64, app_id: &str, owner: lunchbox_api::WindowOwner) -> lunchbox_api::WindowInfo {
        lunchbox_api::WindowInfo {
            id,
            name: None,
            app_id: Some(app_id.into()),
            window_class: None,
            pid: None,
            workspace: Some("1".into()),
            in_scratchpad: false,
            visible: true,
            focused: false,
            owner,
        }
    }

    /// The taskbar lists what the caregiver opened, not Lunchbox's own
    /// furniture: the launcher and the HUD are always mapped, and buttons for
    /// them would be a row that never empties — which is also what the "X"
    /// keys its "leave the mode" state off.
    #[test]
    fn the_taskbar_lists_only_what_the_caregiver_opened() {
        use lunchbox_api::WindowOwner;
        let windows = vec![
            window(1, "com.lunchboxos.launcher", WindowOwner::Lunchbox),
            window(2, "com.lunchboxos.hud", WindowOwner::Lunchbox),
            window(3, "steam", WindowOwner::Unowned),
            window(4, "org.gnome.Nautilus", WindowOwner::Unowned),
        ];
        let listed: Vec<u64> = admin_windows(&windows).iter().map(|w| w.id).collect();
        assert_eq!(listed, vec![3, 4]);

        // With nothing of the caregiver's left, the "X" becomes the way out.
        let only_ours = vec![window(1, "com.lunchboxos.launcher", WindowOwner::Lunchbox)];
        assert!(admin_windows(&only_ours).is_empty());
        assert!(admin_windows_on_screen(&only_ours).is_empty());
    }

    /// A stashed window still gets a taskbar button, and still does not hold
    /// the exit shut. The two questions are separate and both were got wrong at
    /// once when they shared an answer.
    ///
    /// The exit half: the preloaded Steam client sits stashed for the life of
    /// the session and the compositor reports it `Unowned`, because `snap run`
    /// re-execs and the pid the host recorded is not the pid that draws.
    /// `report_unowned_windows` never noticed — it skips the scratchpad — so
    /// the misattribution surfaced here first, as an administrator mode whose
    /// HUD would never offer the way out on any device that preloads Steam.
    ///
    /// The taskbar half, found by driving it the other way: `sway.conf` stashes
    /// every Steam client window at map time, and `for_window` rules are not
    /// part of the admin binding mode, so Steam launched from the picker
    /// vanishes on arrival. Dropping it from the taskbar too left the caregiver
    /// with a running, invisible, signed-out Steam and no local way to reach
    /// it — which is the first thing issue #154 asks for.
    #[test]
    fn a_stashed_window_is_listed_but_is_not_on_screen() {
        use lunchbox_api::WindowOwner;
        let mut stashed = window(3, "steam", WindowOwner::Unowned);
        stashed.in_scratchpad = true;
        stashed.visible = false;
        let windows = vec![
            window(1, "com.lunchboxos.launcher", WindowOwner::Lunchbox),
            stashed,
        ];
        assert_eq!(
            admin_windows(&windows)
                .iter()
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            vec![3],
            "a stashed window needs the button that brings it back"
        );
        assert!(
            admin_windows_on_screen(&windows).is_empty(),
            "the exit has to be reachable with nothing but stashed furniture up"
        );
    }

    /// "Apps" raises the launcher, which in administrator mode is the picker.
    #[test]
    fn the_apps_button_finds_the_launchers_window() {
        use lunchbox_api::WindowOwner;
        let windows = vec![
            window(7, "steam", WindowOwner::Unowned),
            window(9, "com.lunchboxos.launcher", WindowOwner::Lunchbox),
        ];
        assert_eq!(shell_window_id(&windows), Some(9));
        assert_eq!(shell_window_id(&windows[..1]), None);
    }

    #[test]
    fn taskbar_labels_fall_back_and_are_truncated() {
        use lunchbox_api::WindowOwner;
        let mut w = window(1, "org.gnome.Nautilus", WindowOwner::Unowned);
        assert_eq!(
            taskbar_label(&w),
            "org.gnome.Nautilus",
            "app_id when unnamed"
        );

        w.name = Some("Home".into());
        assert_eq!(taskbar_label(&w), "Home", "the title wins");

        // A blank title is not a name; browsers and terminals both produce them.
        w.name = Some("   ".into());
        assert_eq!(taskbar_label(&w), "org.gnome.Nautilus");

        w.name = Some("A very long browser window title that will not fit".into());
        let label = taskbar_label(&w);
        assert!(label.ends_with('…'));
        assert_eq!(label.chars().count(), TASKBAR_LABEL_CHARS + 1);

        // Exactly at the limit keeps every character and gains no ellipsis.
        w.name = Some("x".repeat(TASKBAR_LABEL_CHARS));
        assert_eq!(taskbar_label(&w).chars().count(), TASKBAR_LABEL_CHARS);
    }

    #[test]
    fn scales_px_literals_and_leaves_other_numbers_alone() {
        let css = scale_px_literals(
            "a { padding: 4px 12px; opacity: 0.5; color: rgba(30, 30, 30, 0.95); }",
            1.5,
        );
        assert_eq!(
            css,
            "a { padding: 6px 18px; opacity: 0.5; color: rgba(30, 30, 30, 0.95); }"
        );
    }

    /// Issue #114: the bar and the confirm popover must each state a base
    /// `font-size`. A label that inherits the *theme's* default instead keeps
    /// its logical-pixel size and so renders 1/factor too small once lunchboxd
    /// drops the compositor scale for an XWayland activity — the bug the
    /// warning banner text showed.
    #[test]
    fn text_roots_declare_a_scalable_font_size() {
        for root in [".hud-bar {", ".confirm-close-popover > contents {"] {
            let block = CSS_TEMPLATE
                .split_once(root)
                .and_then(|(_, rest)| rest.split_once('}'))
                .map(|(block, _)| block)
                .unwrap_or_else(|| panic!("{root} rule missing from the stylesheet"));
            assert!(
                block.contains("font-size:"),
                "{root} must set a font-size so labels don't fall back to the theme default"
            );
        }
        // ...and that size has to follow the factor.
        assert!(css_for_scale(2.0).contains("font-size: 28px"));
    }

    /// Issue #114 follow-up: the confirm popover's Cancel / End buttons must
    /// state their own `font-size`, not rely on inheriting the popover surface's
    /// (`> contents`). The GTK theme sets an explicit `font-size` on `button`,
    /// which is more specific than the inherited value and wins the cascade — so
    /// without a rule of its own the button label kept the theme's logical-pixel
    /// size and rendered 1/factor too small under the counter-scale, even though
    /// the box around it grew.
    #[test]
    fn confirm_popover_button_declares_its_own_font_size() {
        let rule = ".confirm-close-popover button {";
        let block = CSS_TEMPLATE
            .split_once(rule)
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(block, _)| block)
            .unwrap_or_else(|| panic!("{rule} rule missing from the stylesheet"));
        assert!(
            block.contains("font-size:"),
            "{rule} must set a font-size so the label scales instead of \
             inheriting the theme's unscaled button font"
        );
    }

    /// Issue #114: the slider knob has to be at least as big as the size the
    /// GTK theme would pick on its own (16px), or the theme wins the cascade at
    /// factor 1.0 and the counter-scaled value comes out smaller than the
    /// un-scaled knob — a shrinking touch target.
    #[test]
    fn slider_knob_is_scaled_from_at_least_the_theme_size() {
        for slider in [".volume-slider slider {", ".brightness-slider slider {"] {
            let block = CSS_TEMPLATE
                .split_once(slider)
                .and_then(|(_, rest)| rest.split_once('}'))
                .map(|(block, _)| block)
                .unwrap_or_else(|| panic!("{slider} rule missing from the stylesheet"));
            for dim in ["min-width", "min-height"] {
                let value: i32 = block
                    .split_once(&format!("{dim}:"))
                    .and_then(|(_, rest)| rest.split_once("px"))
                    .and_then(|(value, _)| value.trim().parse().ok())
                    .unwrap_or_else(|| panic!("{slider} must set {dim} in px"));
                assert!(
                    value >= 16,
                    "{slider} {dim} is {value}px; below the theme's own 16px it does not scale"
                );
            }
        }
    }

    /// Issue #178: the flyout is a text root of its own, so like the bar and
    /// the confirm prompt it has to state a `font-size` — its percentage
    /// readout would otherwise keep the theme's logical-pixel size and render
    /// 1/factor too small under the counter-scale (issue #114's rule).
    #[test]
    fn the_slider_flyout_declares_a_scalable_font_size() {
        let rule = ".slider-popover > contents {";
        let block = CSS_TEMPLATE
            .split_once(rule)
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(block, _)| block)
            .unwrap_or_else(|| panic!("{rule} rule missing from the stylesheet"));
        assert!(
            block.contains("font-size:"),
            "{rule} must set a font-size so the readout does not fall back to \
             the theme default"
        );
    }

    /// Issue #178: neither slider rule may state a *length* floor.
    ///
    /// A CSS minimum is a floor GTK takes the maximum of against the widget's
    /// size request, so a `min-width` here outranks a shorter request — which
    /// is exactly how the vertical bar's swapped rule silently cancelled the
    /// #160 reading-session shortening and left the page-turn buttons clipped
    /// off the bottom of the bar. The length is `BASE_SLIDER_LENGTH`, applied
    /// as a request so it can follow the HUD scale factor.
    #[test]
    fn slider_rules_leave_their_length_to_the_size_request() {
        for rule in [".volume-slider {", ".brightness-slider {"] {
            let block = CSS_TEMPLATE
                .split_once(rule)
                .and_then(|(_, rest)| rest.split_once('}'))
                .map(|(block, _)| block)
                .unwrap_or_else(|| panic!("{rule} rule missing from the stylesheet"));
            let value: i32 = block
                .split_once("min-width:")
                .and_then(|(_, rest)| rest.split_once("px"))
                .and_then(|(value, _)| value.trim().parse().ok())
                .unwrap_or_else(|| panic!("{rule} must state min-width in px"));
            assert_eq!(
                value, 0,
                "{rule} min-width is {value}px, which outranks the slider's \
                 own size request"
            );
        }
    }

    /// The sliders are out of the bar, so nothing in the stylesheet should
    /// still be turning them for the vertical layout. A leftover rule here
    /// would apply to the flyout — a popover is a descendant of the bar icon
    /// it is parented to, so `.hud-vertical` still matches inside it — and
    /// would zero the width of a slider that is horizontal in both layouts.
    #[test]
    fn the_vertical_layout_no_longer_turns_the_sliders() {
        for dead in [
            ".hud-vertical .volume-slider",
            ".hud-vertical .brightness-slider",
            ".hud-vertical .volume-control",
            ".hud-vertical .brightness-control",
        ] {
            // Only selectors count; the explanatory comment above them names
            // the rules deliberately, and naming them is the point.
            let stylesheet: String = CSS_TEMPLATE
                .lines()
                .filter(|line| !line.trim_start().starts_with(['/', '*', '-']))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !stylesheet.contains(dead),
                "{dead} is still in the stylesheet; the sliders left the bar in \
                 issue #178 and a rule that turns them now hits the flyout"
            );
        }
    }
}
