use std::num::NonZeroU16;

use wisp_render::transcript::copy::{DONE, READY};
use wisp_tui::term::sync;

#[derive(Default)]
pub(crate) struct Updates {
    received: usize,
    held: Option<(usize, String)>,
}

impl vt100::Callbacks for Updates {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        first: Option<u8>,
        second: Option<u8>,
        params: &[&[u16]],
        command: char,
    ) {
        match (first, second, params, command) {
            (Some(b'?'), None, &[&[2026]], 'h') if self.held.is_none() => {
                self.held = Some((self.received - sync::begin().len(), screen.contents()));
            }
            (Some(b'?'), None, &[&[2026]], 'l') => self.held = None,
            _ => {}
        }
    }
}

pub(crate) fn process(parser: &mut vt100::Parser<Updates>, bytes: &[u8]) {
    for byte in bytes {
        parser.callbacks_mut().received += 1;
        parser.process(std::slice::from_ref(byte));
    }
}

pub(crate) fn contents(parser: &vt100::Parser<Updates>) -> String {
    parser
        .callbacks()
        .held
        .as_ref()
        .map_or_else(|| parser.screen().contents(), |(_, text)| text.clone())
}

pub(crate) fn complete<'a>(parser: &vt100::Parser<Updates>, bytes: &'a [u8]) -> &'a [u8] {
    let updates = parser.callbacks();
    let end = updates
        .held
        .as_ref()
        .map_or(updates.received, |(at, _)| *at);
    &bytes[..end]
}

pub(crate) fn shown(bytes: &[u8], rows: u16, cols: u16) -> String {
    contents(&parsed(bytes, rows, cols))
}

pub(crate) fn parsed(bytes: &[u8], rows: u16, cols: u16) -> vt100::Parser<Updates> {
    let mut parser = vt100::Parser::new_with_callbacks(
        NonZeroU16::new(rows).unwrap(),
        NonZeroU16::new(cols).unwrap(),
        0,
        Updates::default(),
    );
    process(&mut parser, bytes);
    parser
}

pub(crate) fn idle(shown: &str) -> bool {
    shown
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .find(|line| !line.starts_with(['\u{2570}', '+']))
        .is_some_and(|status| status.contains(DONE) || status.contains(READY))
}
