use wisp_protocol::settings::{AppliedChange, Layer, Owner, SettingTarget, SettingValue, names};

use super::config_screen::Display;
use super::presenter::{GNU_SCREEN_KEEPS, Options, TMUX_MOUSE_OFF, VS_CODE_WHEEL};
use crate::term::caps::{Reach, SESSION_SOURCE};
use crate::term::clipboard::{self, Host};
use crate::term::host::TmuxMouse;
use crate::term::{Caps, Choice, Env, Flags, ProbeReport, SavedDisplay, Streams};

#[derive(Clone, Debug)]
pub struct Kept {
    pub env: Env,
    pub streams: Streams,
    pub flags: Flags,
    pub saved: SavedDisplay,
    pub probe: Option<ProbeReport>,
    pub host: Host,
    pub tmux: Option<TmuxMouse>,
}

impl Kept {
    pub fn resolve(&self) -> Caps {
        Caps::resolve_saved(
            &self.env,
            self.streams,
            self.flags,
            &self.saved,
            self.probe.as_ref(),
        )
    }

    pub fn copy_on_select(&self, caps: &Caps) -> Choice<bool> {
        clipboard::copy_on_release(caps.copy_on_select.clone(), &self.host)
    }

    pub fn display(&self, caps: &Caps) -> Display {
        let mut out = Display::from_caps(caps, &self.copy_on_select(caps));
        if caps.verbose.source == SESSION_SOURCE {
            out.set(
                names::DISPLAY_VERBOSE,
                SettingValue::Bool(caps.verbose.value),
                SESSION_SOURCE,
            );
        }
        for name in out.names() {
            if let Some(value) = self.saved.session(name).filter(|v| *v != "auto") {
                out.set_session(name, value);
            }
        }
        for path in self.saved.section_paths() {
            if let Some(choice) = self.saved.section(&path) {
                out.set_section(&path, choice.value, &choice.source);
            }
        }
        out
    }

    pub fn mouse_notes(&self, mouse: bool) -> Vec<String> {
        let mut out = vec![];
        if mouse && self.tmux == Some(TmuxMouse::Off) {
            out.push(TMUX_MOUSE_OFF.to_owned());
        }
        if !mouse && self.env.vscode() {
            out.push(VS_CODE_WHEEL.to_owned());
        }
        out
    }

    pub fn notes(&self, caps: &Caps) -> Vec<String> {
        let mut out = self.mouse_notes(caps.mouse.value);
        if self.env.present("STY") {
            out.push(GNU_SCREEN_KEEPS.to_owned());
        }
        out
    }

    pub fn options(&self, caps: &Caps) -> Options {
        let mut options = Options::new(caps.theme(), caps.glyphs.value);
        options.shift_enter = caps.kitty_keyboard;
        options.mouse = caps.mouse.value;
        options.motion = caps.motion.value;
        options.verbose = caps.verbose.value;
        options.scroll_speed = caps.scroll_speed.value;
        options.full_repaint = caps.full_repaint.value;
        options.pictures = caps.picture();
        options.notes = self.notes(caps);
        options.display = self.display(caps);
        options.copy_on_release = self.copy_on_select(caps).value;
        options.kept = Some(self.clone());
        options.sidebar = caps.sidebar.value;
        options
    }

    pub fn changed(&mut self, changes: &[AppliedChange]) -> bool {
        let mut any = false;
        for change in changes {
            let spec = change.target.spec();
            if spec.owner != Owner::Client {
                continue;
            }
            let reach = Reach {
                session: change.layer == Layer::Session,
                saved: change.saved.is_some(),
            };
            if reach == Reach::default() {
                continue;
            }
            let value = change.value.as_ref().map(SettingValue::text);
            let name = match &change.target {
                SettingTarget::Path(path) if path.place.section().is_some() => path.to_string(),
                SettingTarget::Name(_) | SettingTarget::Path(_) => spec.name.to_owned(),
            };
            self.saved.changed(&name, value.as_deref(), reach);
            any = true;
        }
        any
    }
}

#[cfg(test)]
mod tests;
