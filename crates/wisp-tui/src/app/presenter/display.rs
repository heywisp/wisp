use wisp_protocol::AgentEvent;
use wisp_protocol::settings::names;
use wisp_render::theme::Token;
use wisp_render::transcript::{Detail, View as TranscriptView};

use super::{AppError, Presenter};
use crate::term::{Caps, Console, Modes};

impl Presenter {
    pub(super) fn display_changed(&mut self, event: &AgentEvent) {
        let AgentEvent::SettingsChanged {
            changes, answers, ..
        } = event
        else {
            return;
        };
        let Some(kept) = &mut self.kept else {
            return;
        };
        if !kept.changed(changes) {
            return;
        }
        let caps = kept.resolve();
        self.apply_display(&caps);
        let changed = |name: &str| changes.iter().any(|c| c.target.spec().name == name);
        if changed(names::DISPLAY_SIDEBAR) {
            self.sidebar.set_setting(caps.sidebar.value);
        }
        let folds = changed(names::DISPLAY_SIDEBAR_FOLDED)
            && self
                .sidebar
                .folds_changed(*answers, caps.sidebar_folded.value);
        let pins = changed(names::DISPLAY_SIDEBAR_PINNED)
            && self.sidebar.set_pinned(caps.sidebar_pinned.value);
        if folds || pins {
            self.frames.mark();
        }
        self.sidebar_changed();
    }

    pub(super) fn apply_display(&mut self, caps: &Caps) {
        let Some(kept) = &self.kept else {
            return;
        };
        let copy = kept.copy_on_select(caps);
        self.display = kept.display(caps);
        let notes = if caps.mouse.value == self.mouse {
            vec![]
        } else {
            kept.mouse_notes(caps.mouse.value)
        };
        let mut repaint = false;
        let mut rewrap = false;
        let theme = caps.theme();
        if theme != self.theme {
            self.theme = theme;
            repaint = true;
        }
        if caps.glyphs.value != self.glyphs {
            self.glyphs = caps.glyphs.value;
            rewrap = true;
        }
        let cx = caps.motion_ctx();
        if cx != self.pacing.ctx() {
            self.pacing.set_ctx(cx);
            self.frames.mark();
        }
        let detail = if caps.verbose.value {
            Detail::Full
        } else {
            Detail::Scrollback
        };
        let view = TranscriptView {
            detail,
            bands: self.theme.fills(Token::Surface),
            mouse: caps.mouse.value,
        };
        if view != self.transcript.view() {
            self.transcript.set_view(view);
            rewrap = true;
        }
        self.scroll_speed = caps.scroll_speed.value.max(1);
        if caps.full_repaint.value != self.painter.always_full() {
            self.painter.set_always_full(caps.full_repaint.value);
            repaint = true;
        }
        let pictures = caps.picture().filter(|_| !self.pictures_lost);
        if pictures != self.pictures {
            self.pictures = pictures;
            self.frames.mark();
        }
        self.select.set_copy_on_release(copy.value);
        if caps.mouse.value != self.mouse {
            self.mouse = caps.mouse.value;
            self.modes_due = Some(caps.mouse.value);
            rewrap = true;
            for note in notes {
                self.view.note(vec![note]);
            }
            self.view.sync(self.transcript.cells().len());
            repaint = true;
        }
        if rewrap {
            self.view.rewrap();
            self.agent_models.set_view(view);
            self.pager_rows.clear();
            repaint = true;
        }
        if repaint {
            self.painter.invalidate();
            if let Some(pager) = &mut self.pager {
                if rewrap {
                    pager.refresh();
                } else {
                    pager.invalidate();
                }
            }
            self.frames.mark();
        }
    }

    pub(super) fn set_modes<C: Console>(&mut self, console: &mut C) -> Result<(), AppError> {
        let Some(mouse) = self.modes_due.take() else {
            return Ok(());
        };
        let modes = Modes {
            mouse,
            ..console.modes()
        };
        if let Err(e) = console.set_modes(modes) {
            return self.halt(console, e);
        }
        Ok(())
    }
}
