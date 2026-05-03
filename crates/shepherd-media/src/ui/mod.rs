//! egui-based browse UI.

mod grid;
mod theme;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use shepherd_media_core::{Session, SessionInput, SessionState};

use crate::platform;
use crate::posters::{self, PosterCache};

/// Why the UI loop returned.
#[derive(Debug, Clone, Copy)]
pub enum ExitCause {
    User,
    Signal,
}

pub fn run(mut session: Session, term: Arc<AtomicBool>) -> Result<ExitCause, eframe::Error> {
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
    focused: usize,
    columns: usize,
    term: Arc<AtomicBool>,
    signaled: Arc<AtomicBool>,
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
            // Keep the egui context ticking so we can react to player events,
            // but don't draw the grid. Painting an empty central panel keeps
            // the window alive on Wayland.
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(egui::Color32::BLACK))
                .show(ctx, |_| {});
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }

        self.handle_keyboard(ctx);
        grid::draw(
            ctx,
            &mut self.session,
            &mut self.focused,
            &mut self.columns,
            &self.posters,
        );

        // Repaint regularly to keep state-machine ticks flowing even when
        // there's no input.
        ctx.request_repaint_after(Duration::from_millis(100));
        let _ = frame;
    }
}

impl BrowseApp {
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let library = self.session.library().clone();
        let n = library.items.len();
        if n == 0 {
            return;
        }

        let cols = self.columns.max(1);
        ctx.input(|input| {
            if input.key_pressed(egui::Key::ArrowRight) {
                self.move_focus(1, n);
            }
            if input.key_pressed(egui::Key::ArrowLeft) {
                self.move_focus_back(1, n);
            }
            if input.key_pressed(egui::Key::ArrowDown) {
                self.move_focus(cols, n);
            }
            if input.key_pressed(egui::Key::ArrowUp) {
                self.move_focus_back(cols, n);
            }
            if input.key_pressed(egui::Key::Enter) {
                self.activate(&library, n);
            }
            if input.key_pressed(egui::Key::Escape) {
                self.session.handle_input(SessionInput::ExitSession);
            }
        });
    }

    fn move_focus(&mut self, step: usize, len: usize) {
        let next = (self.focused + step).min(len.saturating_sub(1));
        self.focused = next;
    }

    fn move_focus_back(&mut self, step: usize, _len: usize) {
        self.focused = self.focused.saturating_sub(step);
    }

    fn activate(&mut self, library: &shepherd_media_core::Library, n: usize) {
        if self.focused >= n {
            return;
        }
        let item = &library.items[self.focused];
        // Don't try to start items that have no source for the current
        // platform; the grid grays them out, but a stray Enter shouldn't
        // emit a confusing warning either.
        let info = platform::current();
        if shepherd_media_core::resolve_source(item, &info).is_none() {
            return;
        }
        self.session
            .handle_input(SessionInput::SelectItem(item.id.clone()));
    }

    fn poll_gamepad(&mut self) {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return;
        };
        // Drain the queue first so we drop the gilrs borrow before mutating
        // self.
        let mut events = Vec::new();
        while let Some(event) = gilrs.next_event() {
            events.push(event.event);
        }
        let library = self.session.library().clone();
        let n = library.items.len();
        let cols = self.columns.max(1);
        for ev in events {
            use gilrs::{Button, EventType};
            if let EventType::ButtonPressed(btn, _) = ev {
                match btn {
                    Button::DPadLeft => self.move_focus_back(1, n),
                    Button::DPadRight => self.move_focus(1, n),
                    Button::DPadUp => self.move_focus_back(cols, n),
                    Button::DPadDown => self.move_focus(cols, n),
                    Button::South => self.activate(&library, n),
                    Button::East => self.session.handle_input(SessionInput::ExitSession),
                    _ => {}
                }
            }
        }
    }
}
