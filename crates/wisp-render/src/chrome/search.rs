#[cfg(test)]
use wisp_protocol::NoSearch;
use wisp_protocol::SearchForm;

use crate::theme::glyphs::Glyphs;
use crate::transcript::copy;

pub use wisp_protocol::opening::{SearchForms, SearchPair};

pub fn search_change(
    from: SearchForm,
    to: SearchForm,
    thinking: bool,
    glyphs: &Glyphs,
) -> Option<String> {
    if !changes_search(from, to) {
        return None;
    }
    let shift = copy::search_shift(glyphs.arrow, from.word(), &to.words());
    Some(if thinking {
        format!("{shift} {} {}", glyphs.separator, copy::REASONING_DROPPED)
    } else {
        shift
    })
}

pub const fn changes_search(from: SearchForm, to: SearchForm) -> bool {
    from.is_native() != to.is_native()
}

pub fn search_row(from: SearchForm, to: SearchForm, glyphs: &Glyphs) -> Option<String> {
    if changes_search(from, to) {
        return Some(copy::search_shift(glyphs.arrow, from.word(), to.word()));
    }
    (!to.is_native()).then(|| copy::search_line(&to.words()))
}

pub fn search_banner(form: SearchForm, chat_only: bool, glyphs: &Glyphs) -> Option<String> {
    if form.is_native() {
        return None;
    }
    let line = copy::search_line(&form.words());
    Some(if chat_only {
        format!("{line} {} {}", glyphs.separator, copy::CHAT_ONLY)
    } else {
        line
    })
}

#[cfg(test)]
mod tests;
