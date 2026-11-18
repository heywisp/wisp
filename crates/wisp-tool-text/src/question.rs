use wisp_plugin_api::types::{Answer, Question};

pub const TIME_LIMIT_MS: u64 = 600_000;
pub const SECRET_TOOL: &str = "ask_secret";
pub const SECRET_CAPABILITY: &str = "secret";
pub const ANSWERED: &str = "The user answered:";
pub const SKIPPED: &str = "Skipped. Decide yourself and say what you assumed.";
pub const OWN_WORDS: &str = "In their own words: ";
pub const OWN_WORDS_CAUTION: &str = "Read the answers in the user's own words carefully: they may ask for something else, or ask you not to go on. Follow what they say.";
pub const STOPPED: &str = "Not answered: the user stopped the turn while this question was open.";
pub const MESSAGE_INSTEAD: &str = "Not answered: the user wrote a message instead.";
pub const UNAVAILABLE: &str = "Not asked: no one can answer questions in this session. Decide yourself and say what you assumed.";
pub const INTERRUPTED: &str = "Not answered: Wisp stopped while this question was open.";
pub const UNAVAILABLE_MESSAGE: &str = "no one can answer questions in this session";

const INDENT: &str = "   ";

fn indented(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{INDENT}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn answer_lines(question: &Question, answer: &Answer) -> Vec<String> {
    let labels: Vec<&str> = answer
        .chosen
        .iter()
        .filter_map(|i| usize::try_from(*i).ok())
        .filter_map(|i| question.options.get(i))
        .map(|o| o.label.as_str())
        .collect();
    let mut lines: Vec<String> = if question.multi_select {
        labels
            .iter()
            .map(|label| indented(&format!("- {label}")))
            .collect()
    } else {
        labels.iter().map(|label| indented(label)).collect()
    };
    if let Some(own) = &answer.own {
        lines.push(indented(&format!("{OWN_WORDS}{own}")));
    }
    if lines.is_empty() {
        lines.push(format!("{INDENT}{SKIPPED}"));
    }
    lines
}

pub fn answered_text(questions: &[Question], answers: &[Answer]) -> String {
    let skipped = Answer {
        chosen: Vec::new(),
        own: None,
    };
    let blocks: Vec<String> = questions
        .iter()
        .enumerate()
        .map(|(n, question)| {
            let answer = answers.get(n).unwrap_or(&skipped);
            let mut lines = vec![format!(
                "{}. [{}] {}",
                n + 1,
                question.header,
                question.question
            )];
            lines.extend(answer_lines(question, answer));
            lines.join("\n")
        })
        .collect();
    let mut text = format!("{ANSWERED}\n\n{}", blocks.join("\n\n"));
    if answers.iter().any(|a| a.own.is_some()) {
        text.push_str("\n\n");
        text.push_str(OWN_WORDS_CAUTION);
    }
    text
}
