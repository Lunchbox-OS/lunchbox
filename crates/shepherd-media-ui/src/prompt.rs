//! The "continue watching" prompt shown over the browse grid.
//!
//! With the opt-in resume feature enabled (`--resume` on Linux, the per-library
//! toggle on Android), re-opening a library offers to pick the last-watched item
//! back up. Both front-ends show the same card, so it lives here beside the
//! shared grid.
//!
//! The prompt owns its focus by index and paints its own buttons rather than
//! using egui's focus traversal, for the same reason [`crate::grid`] does: both
//! front-ends drive this from a D-pad or a gamepad, and a custom-painted
//! two-button row is what makes the focused choice legible from a couch. Pointer
//! taps and the keyboard are handled inside [`ResumePrompt::draw`]; a gamepad is
//! the caller's to translate (see [`ResumePrompt::move_focus`] and
//! [`ResumePrompt::focused_action`]).

use egui::Color32;

use crate::video::format_time;

/// Colors the prompt draws with — each front-end supplies its own theme, as with
/// [`crate::video::OverlayTheme`].
pub struct PromptTheme {
    /// Card background.
    pub panel: Color32,
    /// Heading and button text.
    pub text: Color32,
    /// Secondary line ("Left off at …").
    pub dim_text: Color32,
    /// Unfocused button fill.
    pub button: Color32,
    /// Focused button fill.
    pub button_focused: Color32,
    /// Outline drawn around the focused button.
    pub focus_border: Color32,
}

/// What the viewer chose this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptAction {
    /// Still deciding.
    None,
    /// Resume the offered item.
    Resume,
    /// Dismiss the prompt and browse the library instead.
    Dismiss,
}

/// Which button the D-pad is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Resume,
    Dismiss,
}

/// The card offering to resume the last-watched item, plus its focus state.
///
/// Construct one when the prompt should appear and drop it once it returns
/// something other than [`PromptAction::None`].
#[derive(Default)]
pub struct ResumePrompt {
    focus: Focus,
}

/// Card size (logical px), sized so the title and both buttons read from a
/// couch without dominating a phone screen.
const CARD_SIZE: egui::Vec2 = egui::vec2(720.0, 300.0);
/// Button size, matching the transport overlay's touch targets.
const BUTTON_SIZE: egui::Vec2 = egui::vec2(280.0, 72.0);

impl ResumePrompt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Move the focus between the two buttons. Negative moves left. Used by the
    /// callers' gamepad/D-pad handling; the keyboard is handled in
    /// [`draw`](Self::draw).
    pub fn move_focus(&mut self, delta: i32) {
        if delta < 0 {
            self.focus = Focus::Resume;
        } else if delta > 0 {
            self.focus = Focus::Dismiss;
        }
    }

    /// The action the focused button would take, for a caller mapping a
    /// gamepad's "accept" button onto the prompt.
    pub fn focused_action(&self) -> PromptAction {
        match self.focus {
            Focus::Resume => PromptAction::Resume,
            Focus::Dismiss => PromptAction::Dismiss,
        }
    }

    /// Draw the card centered in `rect`, over a scrim that dims whatever is
    /// behind it, and return the viewer's choice.
    ///
    /// `position` is where the item was left off and `duration` its length when
    /// known; both feed the "Left off at …" line. Handles pointer taps, arrow
    /// keys / Tab (focus), Enter (activate) and Escape / BACK (dismiss).
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        item_title: &str,
        position: Option<f64>,
        duration: Option<f64>,
        theme: &PromptTheme,
    ) -> PromptAction {
        let mut action = PromptAction::None;

        // Keyboard: on Android the remote's D-pad arrives as arrow keys and its
        // centre button as Enter, so this covers both front-ends' remotes.
        ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.focus = Focus::Resume;
            }
            if i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::Tab) {
                self.focus = Focus::Dismiss;
            }
            if i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space) {
                action = self.focused_action();
            }
            if i.key_pressed(egui::Key::Escape)
                || i.key_pressed(egui::Key::Backspace)
                || i.key_pressed(egui::Key::BrowserBack)
            {
                action = PromptAction::Dismiss;
            }
        });

        // Scrim: dim the grid behind so the card is unmistakably modal, and
        // swallow taps outside it (a stray tap on a poster must not start
        // something while a choice is pending).
        ui.allocate_rect(rect, egui::Sense::click());
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(200));

        let card = egui::Rect::from_center_size(rect.center(), CARD_SIZE.min(rect.size() * 0.9));
        painter.rect_filled(card, 20.0, theme.panel);

        let inner = card.shrink(32.0);
        painter.text(
            inner.left_top(),
            egui::Align2::LEFT_TOP,
            "Continue watching",
            egui::FontId::proportional(26.0),
            theme.dim_text,
        );
        painter.text(
            inner.left_top() + egui::vec2(0.0, 48.0),
            egui::Align2::LEFT_TOP,
            elide(item_title, 40),
            egui::FontId::proportional(36.0),
            theme.text,
        );
        if let Some(line) = left_off_line(position, duration) {
            painter.text(
                inner.left_top() + egui::vec2(0.0, 104.0),
                egui::Align2::LEFT_TOP,
                line,
                egui::FontId::proportional(24.0),
                theme.dim_text,
            );
        }

        // Button row along the bottom of the card.
        let row = egui::Rect::from_min_size(
            egui::pos2(inner.left(), inner.bottom() - BUTTON_SIZE.y),
            egui::vec2(inner.width(), BUTTON_SIZE.y),
        );
        let button_width = ((row.width() - 24.0) / 2.0).min(BUTTON_SIZE.x);
        let resume_rect =
            egui::Rect::from_min_size(row.min, egui::vec2(button_width, BUTTON_SIZE.y));
        let dismiss_rect = egui::Rect::from_min_size(
            egui::pos2(resume_rect.right() + 24.0, row.min.y),
            egui::vec2(button_width, BUTTON_SIZE.y),
        );

        if self.button(
            ui,
            resume_rect,
            "▶  Resume",
            self.focus == Focus::Resume,
            theme,
        ) {
            action = PromptAction::Resume;
        }
        if self.button(
            ui,
            dismiss_rect,
            "Library",
            self.focus == Focus::Dismiss,
            theme,
        ) {
            action = PromptAction::Dismiss;
        }

        action
    }

    /// One custom-painted button. Returns whether it was clicked/tapped;
    /// hovering it also moves the focus, so pointer and remote agree on which
    /// choice is highlighted.
    fn button(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        label: &str,
        focused: bool,
        theme: &PromptTheme,
    ) -> bool {
        let response = ui.allocate_rect(rect, egui::Sense::click());
        let fill = if focused || response.hovered() {
            theme.button_focused
        } else {
            theme.button
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 12.0, fill);
        if focused {
            painter.rect_stroke(
                rect,
                12.0,
                egui::Stroke::new(3.0_f32, theme.focus_border),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(24.0),
            theme.text,
        );
        response.clicked()
    }
}

/// "Left off at 12:34 of 1:24:48", or just the position when the length is
/// unknown (a live stream, or a file mpv never measured).
fn left_off_line(position: Option<f64>, duration: Option<f64>) -> Option<String> {
    let position = position?;
    Some(match duration.filter(|d| d.is_finite() && *d > 0.0) {
        Some(duration) => format!(
            "Left off at {} of {}",
            format_time(position),
            format_time(duration)
        ),
        None => format!("Left off at {}", format_time(position)),
    })
}

/// Trim a long title to `max` characters with an ellipsis. The card is a fixed
/// width and the text is painted (not laid out in a `Ui`), so it would otherwise
/// run past the edge.
fn elide(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_starts_on_resume_and_moves_both_ways() {
        let mut p = ResumePrompt::new();
        assert_eq!(p.focused_action(), PromptAction::Resume);
        p.move_focus(1);
        assert_eq!(p.focused_action(), PromptAction::Dismiss);
        p.move_focus(-1);
        assert_eq!(p.focused_action(), PromptAction::Resume);
        p.move_focus(0);
        assert_eq!(p.focused_action(), PromptAction::Resume);
    }

    #[test]
    fn left_off_line_reads_naturally() {
        assert_eq!(
            left_off_line(Some(754.0), Some(5088.0)).as_deref(),
            Some("Left off at 12:34 of 1:24:48")
        );
        assert_eq!(
            left_off_line(Some(754.0), None).as_deref(),
            Some("Left off at 12:34")
        );
        // A last-watched item with no saved position (watched to the end) is
        // still offerable — there is just nothing to say about where it was.
        assert_eq!(left_off_line(None, Some(600.0)), None);
    }

    #[test]
    fn elide_only_trims_long_titles() {
        assert_eq!(elide("Sintel", 40), "Sintel");
        let long = "a".repeat(60);
        let elided = elide(&long, 40);
        assert_eq!(elided.chars().count(), 40);
        assert!(elided.ends_with('…'));
    }
}
