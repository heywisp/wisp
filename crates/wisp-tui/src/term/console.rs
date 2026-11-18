use std::io;

use ratatui_core::layout::Size;

use super::clipboard::Osc52;
use super::guard::{Halted, Terminal, Written};
use super::input::Reader;
use super::modes::Modes;
use super::sync::CursorAfter;

pub const BEL: &[u8] = b"\x07";

pub trait Console {
    fn size(&self) -> io::Result<Size>;
    fn suspend(&mut self) -> io::Result<()>;
    fn reapply(&mut self) -> io::Result<()>;
    fn halted(&self) -> Option<Halted>;
    fn frame(&mut self, body: &[u8], size: Size) -> io::Result<Written>;
    fn clipboard(&mut self, sequence: &Osc52) -> io::Result<Written>;
    fn hand_off(&mut self, body: &[u8]) -> io::Result<Written>;
    fn take_back(&mut self) -> io::Result<()>;
    fn bell(&mut self) -> io::Result<Written>;
    fn modes(&self) -> Modes;
    fn set_modes(&mut self, modes: Modes) -> io::Result<()>;
    fn hung_up(&self) -> bool {
        false
    }
}

pub fn handed_off(modes: Modes) -> Modes {
    Modes {
        mouse: false,
        alternate_scroll_off: None,
        alternate_screen: false,
        ..modes
    }
}

pub struct Live {
    terminal: Terminal,
    reader: Option<Reader>,
    modes: Modes,
    handed: bool,
}

impl Live {
    pub fn new(mut terminal: Terminal, modes: Modes) -> io::Result<Live> {
        terminal.apply(modes)?;
        Ok(Live {
            terminal,
            reader: None,
            modes,
            handed: false,
        })
    }

    pub fn listen(&mut self, reader: Reader) {
        self.reader = Some(reader);
    }

    pub fn terminal(&mut self) -> &mut Terminal {
        &mut self.terminal
    }

    pub fn close(mut self) -> Terminal {
        self.reader = None;
        self.terminal
    }
}

impl Console for Live {
    fn size(&self) -> io::Result<Size> {
        self.terminal.size()
    }

    fn hung_up(&self) -> bool {
        self.terminal.hung_up()
    }

    fn suspend(&mut self) -> io::Result<()> {
        self.terminal.suspend()
    }

    fn reapply(&mut self) -> io::Result<()> {
        self.handed = false;
        self.terminal.apply(self.modes)
    }

    fn halted(&self) -> Option<Halted> {
        self.terminal.halted()
    }

    fn frame(&mut self, body: &[u8], size: Size) -> io::Result<Written> {
        let live = self.terminal.size()?;
        if live.width > 0 && live.height > 0 && live != size {
            return Ok(Written::Resized(live));
        }
        self.terminal.write_frame(body, CursorAfter::Hidden)
    }

    fn clipboard(&mut self, sequence: &Osc52) -> io::Result<Written> {
        self.terminal.write(sequence.bytes())
    }

    fn hand_off(&mut self, body: &[u8]) -> io::Result<Written> {
        self.handed = true;
        self.terminal.apply(handed_off(self.modes))?;
        self.terminal.write(body)
    }

    fn take_back(&mut self) -> io::Result<()> {
        self.reapply()
    }

    fn modes(&self) -> Modes {
        self.modes
    }

    fn set_modes(&mut self, modes: Modes) -> io::Result<()> {
        self.modes = modes;
        if self.handed {
            self.terminal.apply(handed_off(modes))
        } else {
            self.terminal.apply(modes)
        }
    }

    fn bell(&mut self) -> io::Result<Written> {
        self.terminal.write(BEL)
    }
}
