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
use lunchbox_util::EntryId;
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

/// The running activity's own icon, at scale 1.0, from §8 of the branding
/// brief ("app icon 34 px, keyline"). Like `MARK_PX` it is given in prose
/// rather than as a token.
const APP_ICON_PX: i32 = 34;

/// The mark on the bar, at scale 1.0: the size an activity's icon is drawn at,
/// because they stand in the same place and never at the same time — the mark
/// while nothing is running, the activity's own icon while something is.
///
/// §8 gives 26px, in prose rather than as a token. At 26, next to a wordmark
/// set in 18px type, the mark read as a detail of the bar rather than as the
/// thing the bar belongs to.
///
/// The brief names the colour mark; the bar wears the mono one, which is the
/// same drawing in one colour — see `lunchbox_branding::MARK_MONO_WHITE_SVG`.
const MARK_PX: i32 = APP_ICON_PX;

/// An `Image` carrying the mark, sized for a bar at `factor`.
///
/// Two places want one: the idle bar, and administrator mode's "Apps" button.
/// Both are rebuilt by `set_mark_scale` when the HUD scale factor changes.
fn build_mark(factor: f64) -> gtk4::Image {
    let mark = gtk4::Image::from_paintable(mark_texture(factor).as_ref());
    set_mark_scale(&mark, factor);
    mark
}

/// Redraw `mark` for a bar at `factor`.
///
/// The mark is a texture, not a themed icon, so it is re-*rasterized* rather
/// than re-measured — six rounded rectangles and a triangle, drawn at the size
/// they will be shown at, keep their corners at every factor.
///
/// `set_pixel_size`, not `set_size_request`. A `GtkImage` draws what it holds at
/// its *icon size* and centres it in whatever the widget was given, so a size
/// request makes the widget bigger and leaves the mark the size it was — which
/// is how it spent its first few commits rendering at GTK's default 16px while
/// every comment here said 26. Measured on the virtual output, not reasoned
/// about: the drawn mark was 13px across.
fn set_mark_scale(mark: &gtk4::Image, factor: f64) {
    mark.set_paintable(mark_texture(factor).as_ref());
    mark.set_pixel_size((f64::from(MARK_PX) * factor).round() as i32);
}

/// The mark, rasterized for a bar at `factor`.
///
/// An SVG at a known size rather than a paintable GTK can rescale, because the
/// mark is six rounded rectangles and a triangle: rasterized at the size it
/// will be drawn at, its corners stay crisp at every HUD scale factor. The
/// texture is therefore rebuilt when the factor changes, alongside the icon
/// pixel sizes.
///
/// `None` if the SVG cannot be rasterized, which on a device means the
/// gdk-pixbuf SVG loader is missing. The bar then carries no mark and says so
/// once in the log — losing the mark is not worth failing to start the one
/// surface a child cannot get out of.
fn mark_texture(factor: f64) -> Option<gtk4::gdk::Texture> {
    let px = (f64::from(MARK_PX) * factor).round() as i32;
    let bytes = glib::Bytes::from_static(lunchbox_branding::MARK_MONO_WHITE_SVG);
    let stream = gtk4::gio::MemoryInputStream::from_bytes(&bytes);
    match gtk4::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        px,
        px,
        true,
        gtk4::gio::Cancellable::NONE,
    ) {
        Ok(pixbuf) => Some(gtk4::gdk::Texture::for_pixbuf(&pixbuf)),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "cannot rasterize the Lunchbox mark; is the gdk-pixbuf SVG loader installed?"
            );
            None
        }
    }
}

/// How far a popover stands off the control it drops from, at scale 1.0.
///
/// The wedge used to do this by existing: it put twelve pixels between the bar
/// and the panel's keyline, eight of them clear. With it gone the panel
/// overlapped the bar's last four pixels, and two dark edges touching read as
/// one surface. This is a little less than the wedge gave — enough to see the
/// bar end and the panel begin, and not so much that the panel looks unmoored
/// from the button it came out of.
const POPOVER_GAP_PX: f64 = 8.0;

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
/// Whether the bar should be in administrator mode (issue #154).
///
/// Debug builds additionally honour `LUNCHBOX_HUD_DEBUG_FORCE_ADMIN_MODE`, for
/// the same reason the page buttons have a hook: the mode is entered from the
/// companion app or the web interface, over BLE or HTTP, so the headless dev
/// session cannot reach it and its taskbar is the one layout on this bar nobody
/// can look at. The hook only makes the HUD *believe* it — nothing else on the
/// device changes, and the window list is real, because the poll that fills it
/// asks this same question.
///
/// Never compiled into a release build. It cannot grant anything either: every
/// action the mode offers is refused by lunchboxd unless lunchboxd agrees the
/// mode is on.
fn in_admin_mode(actual: bool) -> bool {
    #[cfg(debug_assertions)]
    if std::env::var_os("LUNCHBOX_HUD_DEBUG_FORCE_ADMIN_MODE").is_some() {
        return true;
    }
    actual
}

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
    container: gtk4::CenterBox,
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

/// How long a warning stays on the bar.
///
/// §8 of the branding brief makes the warning a toast — up for a few seconds,
/// then gone — where it used to sit on the bar for the rest of the session.
/// Nothing persistent is lost by that: the countdown carries the warning's own
/// colour for the rest of the session, through the one mapping both of them go
/// through (`theme::Urgency`). What the banner was doing after the first few
/// seconds was standing in the middle of the bar holding a sentence a child had
/// already read.
///
/// **Ten seconds, not the brief's three.** The toast takes the countdown's
/// place rather than sitting beside it, so the three seconds are not only how
/// long the message is up but how long the time remaining is *away* — and three
/// seconds is short for a sentence a new reader is sounding out. Ten is long
/// enough to read twice and still short against the shortest gap between two
/// configured warnings worth having.
///
/// Read against `warning_issued_at`, which the state records once per warning
/// (`EventPayload::WarningIssued`), so a second warning raises a second toast
/// rather than extending the first.
const WARNING_TOAST: Duration = Duration::from_secs(10);

/// Whether a warning raised `since` ago is still on the bar.
///
/// The tick is 500ms, so a toast is up for between ten and ten and a half
/// seconds — and cheaper than a timer of its own that would have to be
/// cancelled on every session change.
fn warning_toast_is_up(since: Duration) -> bool {
    since < WARNING_TOAST
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
                .has_arrow(false)
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
    /// Stand the message's popover off the bar by the same gap the flyouts
    /// use. It is the one popover on this bar that is not placed by
    /// `align_popover_to_button`, so it has to be told separately — and told
    /// again whenever the HUD scale factor changes, since the bar itself is not
    /// rebuilt for that.
    fn set_gap(&self, factor: f64) {
        if let Some(popover) = &self.popover {
            popover.set_offset((POPOVER_GAP_PX * factor).round() as i32, 0);
        }
    }

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

    /// Apply the toast's styling. The class goes on the bar element in both
    /// layouts, and on the popover too when there is one, so the message is
    /// tinted to match the icon that produced it. The class itself comes from
    /// `theme::Urgency`, which the countdown reads too.
    fn set_severity_class(&self, class: Option<&str>) {
        for urgency in crate::theme::Urgency::ALL {
            self.container.remove_css_class(urgency.toast_class());
            if let Some(popover) = &self.popover {
                popover.remove_css_class(urgency.toast_class());
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

    /// Show or hide the label. It is hidden whenever there is no activity to
    /// name: in administrator mode, where the taskbar wants the space (issue
    /// #154), and at the launcher, where the wordmark has already said what the
    /// device is doing (issue #209).
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
/// The guarantee is the vertical bar's only — see `build_title_label` for why
/// the horizontal one gave it up. The ceiling is both bars'.
///
/// Horizontally these were measured, not guessed: the bar is full at 1280
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
    // Empty until there is an activity, rather than "No session": the label is
    // hidden without one, and the text a hidden label holds is the text that
    // shows in the half-second before the first state arrives.
    label.add_css_class("app-name");
    // The left box expands, so without this a long activity name ("Alice's
    // Adventures in Wonderland") takes its natural width and pushes the
    // right-hand controls off the end of the bar — where they are simply
    // clipped, not wrapped. Ellipsizing gives the label a small minimum size
    // so the controls always fit (issue #160 added two more of them).
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    // An ellipsizing label asks for the ellipsis as its *minimum*, and GTK
    // hands out minimums unless a child claims the leftover — so something has
    // to claim it or the name collapses to "..." with hundreds of pixels going
    // spare. `xalign` keeps the text against the start edge either way.
    //
    // On the **rotated** label that claimant is the label itself, set on the
    // child, whose own axes are still the text's: it expands along the text,
    // and the wrapper turns that into vertical expansion.
    //
    // On the **horizontal** bar it is not, any more. An expanding label
    // stretches its allocation across the whole bar, and the countdown packed
    // after it goes to the far end with it — which is where "12:40 left" used
    // to render, three hundred pixels from the name it is about (#209). The
    // slack goes to a spacer of its own instead; see `build_hud_content`.
    label.set_hexpand(vertical);
    label.set_xalign(0.0);
    // The ceiling stops a very long name from crowding out the controls it
    // shares the bar with.
    //
    // The floor that went with it is now the vertical bar's alone. It was
    // there because an ellipsizing label asks for the ellipsis as its minimum
    // and GTK hands out minimums first, so "Alice in Wonderland" rendered as
    // "..." with 400px of the bar unused (#160) — but a floor is also a
    // *natural* width, so "Celeste" reserved twelve characters and the
    // countdown that now sits beside it started forty pixels out (#209). The
    // horizontal bar no longer needs the floor: nothing on it claims the slack
    // any more except the spacer, so the name is allocated its natural width
    // whenever there is room, and shrinks first when there is not — which is
    // the behaviour #160 wanted in the first place.
    let (min_chars, max_chars) = if vertical {
        VERTICAL_TITLE_CHARS
    } else {
        TITLE_CHARS
    };
    if vertical {
        label.set_width_chars(min_chars);
    }
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
    // The scale the *widgets* were last built against. The bar restyles itself
    // for a new factor from its own timer, which is enough for everything on
    // screen at the time — but not for anything hidden: GTK validates a
    // widget's style when it is mapped and leaves it alone while it is not, so
    // a widget that sits out a `HudScaleChanged` comes back wearing the
    // previous factor's sizes (issue #118). The wordmark did exactly that: it
    // is the idle bar's, so it is hidden for the whole of an
    // `xwayland_native_resolution` activity, which is the only thing that
    // changes the factor — and it returned at twice its size.
    //
    // So a factor change rebuilds the bar, the way an orientation change does
    // and for the same reason. The restyle in the bar's own timer stays: it is
    // what corrects the sizes that are widget properties rather than CSS, and
    // on a freshly built bar it runs once, at the new factor.
    let applied_scale = std::rc::Rc::new(std::cell::Cell::new(state.scale_factor()));
    let follow_daemon = pinned_orientation.is_none();
    let window_for_orientation = window.clone();
    let css_for_orientation = css_provider.clone();
    let state_for_orientation = state.clone();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        if !follow_daemon {
            return glib::ControlFlow::Continue;
        }
        let desired = state_for_orientation.orientation();
        let desired_scale = state_for_orientation.scale_factor();
        let turned = desired != applied_orientation.get();
        let rescaled = (desired_scale - applied_scale.get()).abs() > f64::EPSILON;
        if !turned && !rescaled {
            return glib::ControlFlow::Continue;
        }
        tracing::info!(
            before = ?applied_orientation.get(),
            after = ?desired,
            scale_before = applied_scale.get(),
            scale_after = desired_scale,
            "Rebuilding the HUD"
        );
        applied_orientation.set(desired);
        applied_scale.set(desired_scale);

        // The bar is rebuilt rather than restyled, for the reason issue #118
        // documents: GTK validates a widget's style when it is *mapped* and
        // leaves it alone while hidden, so anything currently hidden — the
        // confirm prompts, the warning, the idle bar's own wordmark — would
        // keep the previous sizes and paint at them the next time it is shown.
        // A fresh widget has no cached style. For an orientation change,
        // rebuilding also spares every widget below from having to know how to
        // change its own axis.
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
        // Only an orientation change needs the surface rebuilt; a factor change
        // leaves the bar on the edge it was already on.
        let visible = window_for_orientation.is_visible();
        if turned {
            window_for_orientation.set_visible(false);
            apply_anchors(&window_for_orientation, desired);
        }
        apply_scale(
            &css_for_orientation,
            &window_for_orientation,
            thickness,
            desired_scale,
            desired,
        );
        if turned {
            window_for_orientation.set_visible(visible);
        }

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
    // A `CenterBox` rather than a `Box`, because the countdown sits in the
    // *middle of the bar* the way a desktop shell puts the clock there — and a
    // box cannot do that. Extra space in a box is shared between the children
    // that claim it, so a centred child lands half the difference between the
    // two side groups away from the real centre, and the countdown would drift
    // as the activity's name grew. A `CenterBox` centres its middle child
    // against the whole bar and gives the sides what is left.
    let container = gtk4::CenterBox::builder()
        .orientation(orientation.flow())
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

    // The mark's end of the bar: the mark, the wordmark or the running
    // activity's icon and name, and the page-turn buttons ahead of them.
    //
    // `halign(Fill)`, not `Start`: with `Start` the box is allocated its
    // *minimum* width and merely positioned left, which collapses the
    // ellipsizing label below to the ellipsis even when the bar has hundreds
    // of pixels to spare. Filling gives the name the leftover room, and the
    // ellipsis then only appears when the bar is genuinely full.
    //
    // The vertical bar wants the other thing on its own axis: this group is the
    // `CenterBox`'s *end*, so it should hug the bottom edge the way the controls
    // hug the top. Filling instead left a lone mark floating in the middle of
    // the bottom half, at the top of an allocation nothing else was using.
    let left_box = gtk4::Box::builder()
        .orientation(orientation.flow())
        .spacing(12)
        .hexpand(!vertical)
        .vexpand(vertical)
        .halign(gtk4::Align::Fill)
        .valign(if vertical {
            gtk4::Align::End
        } else {
            gtk4::Align::Fill
        })
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

    // The mark and the wordmark: the idle bar, and only the idle bar (§8 of the
    // branding brief). In an activity the mark gives its place to the
    // activity's own icon, which is the more useful thing to put there — the
    // child knows whose device it is, and what they want from this end of the
    // bar is what is running.
    //
    // That also settles where the mark goes. The brief puts it at the very left
    // of the bar, where the page-turn buttons are; those stay, because they are
    // the two controls a child touches on every page and the reason they are
    // there is to be as far as possible from the two that throw the session
    // away. Nothing is given up by it: the page buttons belong to a reading
    // session and the mark to no session at all, so the two can no longer be on
    // the bar at the same time.
    let mark = build_mark(1.0);
    orientation.flow_append(&left_box, &mark);

    // "Lunchbox", carried only while no activity is running: in an activity the
    // app's own name takes this end of the bar, and two names would be one too
    // many.
    //
    // **Horizontal only.** A word does not fit across the bar when it is on
    // its side, and one read sideways is worse than none — so the vertical bar
    // wears the mark alone, the way a lid stamped on its edge would. Shrinking
    // the wordmark instead was not enough: at the caption size it still
    // measured sixty pixels across a bar that wants to be fifty, and it made
    // the whole bar that wide.
    //
    // The brief puts a "No session" chip beside it. There is deliberately none:
    // a bar reading `[mark] Lunchbox` over the launcher's own field is already
    // a device with nothing running, and a label saying so is a caption on a
    // picture of itself. What the chip was really fixing is that "No session"
    // used to sit in the *title's* place, where it read as the name of an
    // activity — and the title is simply empty now.
    let wordmark = gtk4::Label::new(Some("Lunchbox"));
    wordmark.add_css_class("hud-wordmark");
    orientation.flow_append(&left_box, &wordmark);

    // The running activity's icon, keylined, between the mark and the name
    // (§8 of the brief). Drawn by the same widget the launcher draws its
    // 64px icons with; the keyline comes out cream here rather than ink,
    // because the stylesheet says so and an ink keyline on an ink bar would be
    // no keyline at all.
    let app_icon = lunchbox_widgets::IconArt::new();
    app_icon.add_css_class("hud-app-icon");
    app_icon.set_size_request(APP_ICON_PX, APP_ICON_PX);
    app_icon.set_keyline(lunchbox_branding::tokens::STROKE_KEYLINE, 1.0);
    app_icon.set_visible(false);
    orientation.flow_append(&left_box, &app_icon);

    let app_label = build_title_label(orientation);
    orientation.flow_append(&left_box, &app_label.widget());

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
    //
    // It carries the mark rather than the word "Apps". Administrator mode is
    // the one place on this device with a taskbar, and a shell's home button is
    // the thing the shell is called — which here is a lunchbox. It is also the
    // only place the mark appears while a session is running, and it earns that
    // by being a button rather than a decoration.
    let apps_mark = build_mark(1.0);
    let start_button = gtk4::Button::builder()
        .child(&apps_mark)
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

    // The middle of the bar, which holds exactly one thing at a time: the
    // countdown, or the message that has taken its place. A warning is the only
    // thing on this bar more important than how long is left, so it says so by
    // standing where the countdown stands rather than by appearing beside it.
    let centre_box = gtk4::Box::builder()
        .orientation(orientation.flow())
        .spacing(0)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .build();
    centre_box.add_css_class("hud-centre");

    let time_display = TimeDisplay::new();
    time_display.set_compact(vertical);
    centre_box.append(&time_display);

    let warning = WarningBanner::build(orientation);
    warning.set_gap(1.0);
    let warning_box = warning.container.clone();
    let warning_icon = warning.icon.clone();
    centre_box.append(&warning_box);

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
    let analog_clock = vertical.then(|| {
        let face = lunchbox_widgets::ClockFace::now(lunchbox_widgets::clock_face::HUD_DIAMETER);
        face.add_css_class("analog-clock");
        face
    });
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

    orientation.flow_sections(&container, &left_box, &centre_box, &right_box);

    // Set up state updates
    let app_label_clone = app_label.clone();
    let mark_for_timer = mark.clone();
    // Both of them: the idle bar's, and the one on administrator mode's "Apps"
    // button. A mark left out here would keep the previous factor's size.
    let marks_for_scale = [mark.clone(), apps_mark.clone()];
    // The wall clock moves between the end of the bar and the middle of it, so
    // the tick needs both boxes and a memory of where it currently is. Moved
    // rather than duplicated: two clocks would be two things to keep wound.
    let clock_box_for_timer = clock_box.clone();
    let centre_box_for_timer = centre_box.clone();
    let right_box_for_clock = right_box.clone();
    let clock_is_centred = std::rc::Rc::new(std::cell::Cell::new(false));
    let app_icon_for_timer = app_icon.clone();
    // The icon the bar is currently showing, so a session that has not changed
    // does not re-resolve its icon twice a second — a theme lookup and possibly
    // a file read.
    let shown_icon_for: std::rc::Rc<std::cell::RefCell<Option<EntryId>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let wordmark_for_timer = wordmark.clone();
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
    // are constructed with the right spacing already. So is the bar itself,
    // which is a `CenterBox` and has no spacing to scale: what used to be its
    // 16px is now the centre section's margin, which is in the stylesheet and
    // so scales with everything else there.
    let scaled_boxes: [(gtk4::Box, i32); 6] = [
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
            for mark in &marks_for_scale {
                set_mark_scale(mark, desired_scale);
            }
            warning_for_timer.set_gap(desired_scale);
            // The activity icon is a paintable GTK rescales, so only its slot
            // and its keyline follow the factor. Dropping the remembered id
            // makes the next tick re-resolve it at the new size, which is what
            // picks a larger icon out of the theme rather than enlarging a
            // small one.
            let icon_px = (f64::from(APP_ICON_PX) * desired_scale).round() as i32;
            app_icon_for_timer.set_size_request(icon_px, icon_px);
            app_icon_for_timer.set_keyline(
                lunchbox_branding::tokens::STROKE_KEYLINE * desired_scale,
                desired_scale,
            );
            shown_icon_for.borrow_mut().take();
            if let Some(face) = &analog_clock_for_scale {
                face.set_diameter(
                    (f64::from(lunchbox_widgets::clock_face::HUD_DIAMETER) * desired_scale).round()
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

        let admin_mode = in_admin_mode(state.admin_mode());

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
        // The mark and the wordmark are the *idle* bar: an activity's own icon
        // and name replace them, and two of either would be one too many. In
        // administrator mode the mark is on the "Apps" button instead, and the
        // taskbar wants this end of the bar.
        mark_for_timer.set_visible(!has_session && !admin_mode);
        wordmark_for_timer.set_visible(!has_session && !admin_mode && !vertical);

        // The running activity's icon. Resolved when the session changes rather
        // than twice a second: a lookup can reach the icon theme and a file on
        // disk, and neither answer changes while the same activity runs.
        //
        // `want` is `None` until the entry is actually known, so a session that
        // started before the first snapshot arrived shows its name now and
        // gains its icon on the tick after the entries do, rather than being
        // remembered as "resolved to nothing".
        let running = match &session_state {
            SessionState::Active { entry_id, .. } | SessionState::Warning { entry_id, .. } => {
                state.entry(entry_id)
            }
            SessionState::NoSession | SessionState::Ending { .. } => None,
        };
        let want = running.as_ref().map(|entry| entry.entry_id.clone());
        if *shown_icon_for.borrow() != want {
            match &running {
                Some(entry) => {
                    let px =
                        (f64::from(APP_ICON_PX) * applied_scale_for_timer.get()).round() as i32;
                    app_icon_for_timer.set_icon(lunchbox_widgets::resolve_icon(entry, px), px);
                }
                None => app_icon_for_timer.set_icon(None, 0),
            }
            *shown_icon_for.borrow_mut() = want;
        }
        // Administrator mode wants this end of the bar for the window list, the
        // same way it takes the title and the wordmark.
        app_icon_for_timer.set_visible(shown_icon_for.borrow().is_some() && !admin_mode);
        // Whether the middle of the bar is showing a message rather than the
        // countdown. The two share that space and only one of them is ever in
        // it: a warning is the only thing on this bar more important than how
        // long is left, so it says so by standing where the countdown stands.
        let message_in_the_centre = match &session_state {
            SessionState::NoSession => {
                // A title with no activity to name is blank, rather than
                // filled with a sentence about being blank. The wordmark and
                // the field below it already say what the device is doing.
                app_label_clone.set_text("");
                time_display_clone.set_remaining(None);
                time_display_clone.set_urgency(None);
                warning_for_timer.set_visible(false);
                false
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
                // No warning has fired for this session yet, so the countdown
                // is the bar's ordinary cream. It is told; it no longer decides
                // (see `TimeDisplay::set_urgency`).
                time_display_clone.set_urgency(None);
                warning_for_timer.set_visible(false);
                false
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

                // One mapping for both, so the pill and the countdown cannot
                // disagree about how loud this is (`theme::Urgency`).
                let urgency = crate::theme::Urgency::from_severity(*severity);
                warning_for_timer.set_severity_class(Some(urgency.toast_class()));
                time_display_clone.set_urgency(Some(urgency));

                // A toast, not a banner: up for a few seconds from the moment
                // the warning was issued, then back to the countdown. The state
                // stays `Warning` for the rest of the session, which is what
                // keeps the countdown in this warning's colour once the toast
                // has gone.
                let up = warning_toast_is_up(warning_issued_at.elapsed());
                warning_for_timer.set_visible(up);
                up
            }
            SessionState::Ending { reason, .. } => {
                app_label_clone.set_text("Session ending...");
                warning_for_timer.set_text(reason);
                warning_for_timer.set_visible(true);
                true
            }
        };

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
        // administrator mode the idle bar's wordmark and blank countdown say
        // nothing, and the space is wanted for the window list.
        taskbar_box_clone.set_visible(admin_mode);
        // The title is hidden without a session as well as in administrator
        // mode. On the vertical bar it reserves twelve characters of width
        // whatever it holds (see `TITLE_CHARS`), so an empty one would leave a
        // hole after the wordmark.
        app_label_clone.set_visible(!admin_mode && has_session);
        // The countdown stands down while a message is in its place.
        let countdown_in_the_centre =
            !admin_mode && !message_in_the_centre && time_display_clone.has_remaining();
        time_display_clone.set_visible(!admin_mode && !message_in_the_centre);

        // And the wall clock takes the middle when neither of them wants it —
        // which is most of the time, because most of the time nothing is
        // running. A bar with its one remaining readout hard against the
        // controls looks like a bar that lost something; a desktop shell puts
        // the clock in the middle for the same reason.
        //
        // The widget is moved rather than duplicated. Reparenting it means
        // putting it back where it came from, and "where it came from" is not
        // the same end of the box in both layouts: `flow_append` appends along
        // a horizontal bar and prepends along a vertical one, so the clock is
        // the first child of the controls in one and the last in the other.
        let centre_is_free = !message_in_the_centre && !countdown_in_the_centre;
        if centre_is_free != clock_is_centred.get() {
            if centre_is_free {
                right_box_for_clock.remove(&clock_box_for_timer);
                centre_box_for_timer.append(&clock_box_for_timer);
            } else {
                centre_box_for_timer.remove(&clock_box_for_timer);
                if vertical {
                    right_box_for_clock.append(&clock_box_for_timer);
                } else {
                    right_box_for_clock.prepend(&clock_box_for_timer);
                }
            }
            clock_is_centred.set(centre_is_free);
        }
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
    popover.set_has_arrow(false);
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
    popover.set_has_arrow(false);
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
    // other axis. The gap is on the axis the popover drops *along*, which is
    // the other one again: down from a horizontal bar, out to the right of a
    // vertical one.
    let gap = (POPOVER_GAP_PX * factor).round() as i32;
    if orientation.is_vertical() {
        popover.set_offset(gap, -offset);
    } else {
        popover.set_offset(offset, gap);
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
    provider.load_from_data(&crate::theme::css_for_scale(factor));
}

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
            if !in_admin_mode(state.admin_mode()) {
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

    /// The toast is up from the moment the warning was issued and gone a few
    /// ticks later. The bar checks this every 500ms, so the boundary only has
    /// to be right to within a tick.
    #[test]
    fn a_warning_is_a_toast_rather_than_a_banner() {
        assert!(warning_toast_is_up(Duration::from_millis(0)));
        assert!(warning_toast_is_up(Duration::from_millis(9_500)));
        assert!(!warning_toast_is_up(Duration::from_secs(10)));
        // A warning issued ten minutes ago is not still on the bar, which is
        // the whole of the change: the session's state stays `Warning` until
        // it ends, and the countdown carries this warning's own colour from
        // then on.
        assert!(!warning_toast_is_up(Duration::from_secs(600)));
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
}
