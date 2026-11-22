use wisp_protocol::{Answer, Question, QuestionEnd};
pub use wisp_tool_text::question::{
    ANSWERED, BUSY_MESSAGE, CLOSED_NOTE, INTERRUPTED, MESSAGE_INSTEAD, NOTHING_OPEN,
    NOTHING_OPEN_MESSAGE, OWN_WORDS, OWN_WORDS_CAUTION, QUESTION_TOOL, SKIPPED, STOPPED,
    UNAVAILABLE, UNAVAILABLE_MESSAGE, WITHDRAW_FIELD, WITHDRAWN, busy, posted,
};

use crate::plugin::convert;

pub fn answered_text(questions: &[Question], answers: &[Answer]) -> String {
    let questions: Vec<_> = questions.iter().map(convert::question_to_api).collect();
    let answers: Vec<_> = answers.iter().map(convert::answer_to_api).collect();
    wisp_tool_text::question::answered_text(&questions, &answers)
}

pub fn result_text(questions: &[Question], how: &QuestionEnd) -> String {
    match how {
        QuestionEnd::Answered { answers } => answered_text(questions, answers),
        QuestionEnd::Stopped => STOPPED.to_owned(),
        QuestionEnd::MessageInstead => MESSAGE_INSTEAD.to_owned(),
        QuestionEnd::Interrupted => INTERRUPTED.to_owned(),
        QuestionEnd::Withdrawn => WITHDRAWN.to_owned(),
    }
}

#[cfg(test)]
mod tests;
