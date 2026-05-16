//! egui-based browse UI.

mod grid;
mod theme;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use shepherd_media_core::{
    ClassifiedUri, Item, Session, SessionInput, SessionState, resolve_source,
};

use crate::platform;
use crate::posters::{self, PosterCache};
use crate::video_cache::VideoCache;

/// Why the UI loop returned.
#[derive(Debug, Clone, Copy)]
pub enum ExitCause {
    User,
    Signal,
}

pub fn run(
    mut session: Session,
    term: Arc<AtomicBool>,
    online: Arc<AtomicBool>,
    cache: Option<Arc<VideoCache>>,
) -> Result<ExitCause, eframe::Error> {
    let posters = posters::prefetch(session.library());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_fullscreen(true)
            .with_decorations(false)
            .with_title("shepherd-media"),
        ..Default::default()
    };

    session.announce_ready();

    let signaled = Arc::new(AtomicBool::new(false));
    let signaled_clone = signaled.clone();

    eframe::run_native(
        "shepherd-media",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(BrowseApp {
                session,
                posters,
                gilrs: gilrs::Gilrs::new().ok(),
                focused: 0,
                columns: 4,
                term,
                signaled: signaled_clone,
                online,
                cache,
            }))
        }),
    )?;

    Ok(if signaled.load(Ordering::SeqCst) {
        ExitCause::Signal
    } else {
        ExitCause::User
    })
}

struct BrowseApp {
    session: Session,
    posters: PosterCache,
    gilrs: Option<gilrs::Gilrs>,
    /// Index into the *visible* item list for the current frame.
    focused: usize,
    columns: usize,
    term: Arc<AtomicBool>,
    signaled: Arc<AtomicBool>,
    /// Latest connectivity status from the background check thread.
    /// `true` when online or when no connectivity check is configured.
    online: Arc<AtomicBool>,
    /// Video cache used to determine which remote items are available offline.
    cache: Option<Arc<VideoCache>>,
}

impl BrowseApp {
    /// Build the list of items to display for the current frame.
    ///
    /// When online, every item with a source for this platform is shown.
    /// When offline, only items that can be played without network access are
    /// shown: local-filesystem sources and previously cached remote items.
    fn visible_items(&self) -> Vec<Item> {
        let info = platform::current();
        let online = self.online.load(Ordering::Relaxed);
        self.session
            .library()
            .items
            .iter()
            .filter(|item| {
                let Some(source) = resolve_source(item, &info) else {
                    return false;
                };
                if online {
                    return true;
                }
                // Offline: only show items we can play without the network.
                match &source.uri {
                    ClassifiedUri::Local(_) => true,
                    _ => self
                        .cache
                        .as_deref()
                        .map(|c| c.cached_path(&item.id).is_some())
                        .unwrap_or(false),
                }
            })
            .cloned()
            .collect()
    }
}

impl eframe::App for BrowseApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Drain SIGTERM.
        if self.term.swap(false, Ordering::SeqCst) {
            self.session.handle_input(SessionInput::SignalTerminate);
            self.signaled.store(true, Ordering::SeqCst);
        }

        self.session.tick();
        self.poll_gamepad();

        if self.session.is_exiting() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // While playback is active, hide the UI and let mpv own the screen.
        if matches!(
            self.session.state(),
            SessionState::Playing { .. } | SessionState::Stopping { .. }
        ) {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::BLACK))
                .show(ctx, |_| {});
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }

        let visible = self.visible_items();
        // Clamp focus to the visible set (it may shrink when going offline).
        if !visible.is_empty() {
            self.focused = self.focused.min(visible.len() - 1);
        }

        self.handle_keyboard(ctx, &visible);
        grid::draw(
            ctx,
            &mut self.session,
            &visible,
            &mut self.focused,
            &mut self.columns,
            &self.posters,
        );

        ctx.request_repaint_after(Duration::from_millis(100));
        let _ = frame;
    }
}

impl BrowseApp {
    fn handle_keyboard(&mut self, ctx: &egui::Context, visible: &[Item]) {
        let n = visible.len();
        if n == 0 {
            return;
        }

        let cols = self.columns.max(1);
        ctx.input(|input| {
            if input.key_pressed(egui::Key::ArrowRight) {
                self.move_focus(1, n);
            }
            if input.key_pressed(egui::Key::ArrowLeft) {
                self.move_focus_back(1);
            }
            if input.key_pressed(egui::Key::ArrowDown) {
                self.move_focus(cols, n);
            }
            if input.key_pressed(egui::Key::ArrowUp) {
                self.move_focus_back(cols);
            }
            if input.key_pressed(egui::Key::Enter) {
                self.activate(visible);
            }
            if input.key_pressed(egui::Key::Escape) {
                self.session.handle_input(SessionInput::ExitSession);
            }
        });
    }

    fn move_focus(&mut self, step: usize, len: usize) {
        self.focused = (self.focused + step).min(len.saturating_sub(1));
    }

    fn move_focus_back(&mut self, step: usize) {
        self.focused = self.focused.saturating_sub(step);
    }

    fn activate(&mut self, visible: &[Item]) {
        let Some(item) = visible.get(self.focused) else {
            return;
        };
        let info = platform::current();
        if resolve_source(item, &info).is_none() {
            return;
        }
        self.session
            .handle_input(SessionInput::SelectItem(item.id.clone()));
    }

    fn poll_gamepad(&mut self) {
        // Collect events inside a block so the mutable borrow on self.gilrs
        // ends before we call self.visible_items().
        let events: Vec<gilrs::EventType> = {
            let Some(gilrs) = self.gilrs.as_mut() else {
                return;
            };
            let mut ev = Vec::new();
            while let Some(event) = gilrs.next_event() {
                ev.push(event.event);
            }
            ev
        };
        if events.is_empty() {
            return;
        }
        let visible = self.visible_items();
        let n = visible.len();
        let cols = self.columns.max(1);
        for ev in events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                match btn {
                    Button::DPadLeft => self.move_focus_back(1),
                    Button::DPadRight => self.move_focus(1, n),
                    Button::DPadUp => self.move_focus_back(cols),
                    Button::DPadDown => self.move_focus(cols, n),
                    Button::South => self.activate(&visible),
                    Button::East => self.session.handle_input(SessionInput::ExitSession),
                    _ => {}
                }
            }
        }
    }
}
