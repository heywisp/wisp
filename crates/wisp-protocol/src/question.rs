use serde::{Deserialize, Serialize};

use crate::RejectKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<QuestionSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AskedBy {
    Server {
        name: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionSource {
    pub asked_by: AskedBy,
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub typed: bool,
}

impl Question {
    pub fn is_typed(&self) -> bool {
        self.source.as_ref().is_some_and(|source| source.typed)
    }

    pub fn kept_from_model(&self) -> bool {
        self.source.as_ref().is_some_and(|source| {
            matches!(source.asked_by, AskedBy::Server { .. } | AskedBy::Unknown)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub chosen: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub own: Option<String>,
}

impl Answer {
    pub fn choose(index: u32) -> Self {
        Self {
            chosen: vec![index],
            own: None,
        }
    }

    pub fn own(text: impl Into<String>) -> Self {
        Self {
            chosen: Vec::new(),
            own: Some(text.into()),
        }
    }

    pub fn skipped() -> Self {
        Self::default()
    }

    pub fn is_skipped(&self) -> bool {
        self.chosen.is_empty() && self.own.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionEnd {
    Answered { answers: Vec<Answer> },
    Stopped,
    MessageInstead,
    Interrupted,
    Withdrawn,
}

pub fn answer_refusal(kind: &RejectKind, call_id: &str, why: Option<&str>) -> String {
    match (kind, why) {
        (RejectKind::BadAnswer, Some(why)) => format!("answer for {call_id} refused: {why}"),
        (RejectKind::BadAnswer, None) => format!("answer for {call_id} refused"),
        _ => format!("no question {call_id} is open"),
    }
}

pub fn secret_refusal(kind: &RejectKind, call_id: &str) -> String {
    match kind {
        RejectKind::BadAnswer => format!("secret for {call_id} refused"),
        _ => format!("no secret request {call_id} is open"),
    }
}

pub fn report_refusal(call_id: &str) -> String {
    format!("no call {call_id} is waiting for a screen report")
}

pub fn check_answers(questions: &[Question], answers: &[Answer]) -> Result<(), String> {
    if answers.len() != questions.len() {
        let unit = if questions.len() == 1 {
            "answer"
        } else {
            "answers"
        };
        return Err(format!(
            "expected {} {unit}, one per question, but got {}",
            questions.len(),
            answers.len()
        ));
    }
    for (n, (question, answer)) in questions.iter().zip(answers).enumerate() {
        let n = n + 1;
        let options = question.options.len();
        if let Some(index) = answer
            .chosen
            .iter()
            .find(|i| usize::try_from(**i).ok().is_none_or(|i| i >= options))
        {
            return Err(format!(
                "answer {n} chooses option {index}, but question {n} has {options} options"
            ));
        }
        if answer.chosen.windows(2).any(|w| w[0] >= w[1]) {
            return Err(format!(
                "answer {n} must list its choices in order, each once"
            ));
        }
        if !question.multi_select && answer.chosen.len() > 1 {
            return Err(format!(
                "question {n} takes one choice, but answer {n} has {}",
                answer.chosen.len()
            ));
        }
        if !question.multi_select && !answer.chosen.is_empty() && answer.own.is_some() {
            return Err(format!(
                "question {n} takes a choice or your own answer, not both"
            ));
        }
        if answer.own.as_deref().is_some_and(|t| t.trim().is_empty()) {
            return Err(format!("answer {n}'s own text is blank"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(multi_select: bool) -> Question {
        Question {
            header: "H".into(),
            question: "Q?".into(),
            options: ["a", "b", "c"]
                .into_iter()
                .map(|label| QuestionOption {
                    label: label.into(),
                    description: String::new(),
                })
                .collect(),
            multi_select,
            source: None,
        }
    }

    fn with(chosen: &[u32], own: Option<&str>) -> Answer {
        Answer {
            chosen: chosen.to_vec(),
            own: own.map(str::to_owned),
        }
    }

    #[test]
    fn a_multi_select_answer_takes_sorted_unique_choices_and_own_words() {
        let questions = [question(true)];
        let check = |answer: Answer| check_answers(&questions, &[answer]);
        assert_eq!(check(with(&[0, 2], Some("and d"))), Ok(()));
        assert_eq!(check(with(&[], None)), Ok(()));
        assert_eq!(check(with(&[0, 1, 2], None)), Ok(()));
        let unsorted = Err("answer 1 must list its choices in order, each once".to_owned());
        assert_eq!(check(with(&[2, 0], None)), unsorted);
        assert_eq!(check(with(&[1, 1], None)), unsorted);
        assert_eq!(
            check(with(&[3], None)),
            Err("answer 1 chooses option 3, but question 1 has 3 options".to_owned())
        );
        assert_eq!(
            check(with(&[u32::MAX], None)),
            Err(format!(
                "answer 1 chooses option {}, but question 1 has 3 options",
                u32::MAX
            ))
        );
        assert_eq!(
            check(with(&[0], Some("\t"))),
            Err("answer 1's own text is blank".to_owned())
        );
    }

    #[test]
    fn answers_are_checked_question_by_question_in_order() {
        let questions = [question(false), question(true)];
        assert_eq!(
            check_answers(&questions, &[with(&[1], None), with(&[0, 2], None)]),
            Ok(())
        );
        assert_eq!(
            check_answers(&questions, &[with(&[0, 2], None), with(&[1], None)]),
            Err("question 1 takes one choice, but answer 1 has 2".to_owned())
        );
        assert_eq!(
            check_answers(&questions, &[with(&[], None), with(&[], Some(" "))]),
            Err("answer 2's own text is blank".to_owned())
        );
    }
}
