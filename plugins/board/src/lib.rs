mod apply;
mod describe;
mod input;
mod model;
mod rights;
mod store;
mod words;

#[cfg(target_arch = "wasm32")]
mod bindings;
#[cfg(target_arch = "wasm32")]
mod glue;

use std::path::Path;

use jiff::Timestamp;

pub use describe::{Def, definitions};
pub use input::{CardInput, Name, READ, Request, WRITE, Write};
pub use model::{Board, Card, Edit, LogLine};
pub use words::{NO_TREE, Shown, shown};

pub const LIMIT_MS: u64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    pub path: String,
    pub session: String,
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub path: String,
    pub session: String,
    pub ended: bool,
}

pub trait Host {
    fn me(&self) -> Option<Me>;
    fn agents(&self) -> Vec<Agent>;
    fn now(&self) -> Timestamp;
    fn offset(&self, at: Timestamp) -> i32;
    fn data_dir(&self) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    View(String),
    Line(String),
    Refused(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    Rule(String),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub request: Request,
    pub shown: Shown,
}

pub fn prepare(tool: &str, input: &str) -> Result<Call, String> {
    let map = input::object(input)?;
    let shown = shown(tool, &map).ok_or_else(|| wisp_tool_text::output::WRONG_INPUT.to_owned());
    let request = input::request(tool, map)?;
    Ok(Call {
        request,
        shown: shown?,
    })
}

pub fn rejected(tool: &str, input: &str) -> Option<Shown> {
    shown(tool, &input::object(input).ok()?)
}

pub fn run(host: &dyn Host, request: &Request) -> Reply {
    reply(answer(host, request))
}

fn reply(answered: Result<Reply, Refusal>) -> Reply {
    match answered {
        Ok(reply) => reply,
        Err(Refusal::Rule(sentence)) => Reply::Refused(sentence),
        Err(Refusal::Failed(sentence)) => Reply::Failed(sentence),
    }
}

fn members(host: &dyn Host, me: &Me) -> Vec<Agent> {
    let mut agents = host.agents();
    agents.push(Agent {
        path: model::ROOT.to_owned(),
        session: me.root.clone(),
        ended: false,
    });
    agents
}

fn answer(host: &dyn Host, request: &Request) -> Result<Reply, Refusal> {
    let me = host
        .me()
        .ok_or_else(|| Refusal::Failed(NO_TREE.to_owned()))?;
    let data = host.data_dir().map_err(Refusal::Failed)?;
    let boards = Path::new(&data).join("boards");
    match request {
        Request::Read { board: None, .. } => {
            let tree = boards.join(&me.root);
            let listed = store::boards(&tree)?;
            Ok(Reply::View(words::list(
                &me.root,
                &listed,
                &members(host, &me),
            )))
        }
        Request::Read {
            board: Some(name),
            card,
        } => {
            let tree = name.tree.as_deref().filter(|tree| *tree != me.root);
            let folder = boards.join(tree.unwrap_or(&me.root));
            let given = name.given();
            let Some(board) = store::newest(&folder, &name.name, &given)?.and_then(|v| v.live())
            else {
                return Err(Refusal::Rule(words::no_board(&given)));
            };
            let agents = members(host, &me);
            let marks = tree.is_none().then_some(agents.as_slice());
            let head = words::head(&name.name, tree);
            match card {
                None => Ok(Reply::View(words::board_view(&head, &board, marks))),
                Some(id) => match board.card(id) {
                    Some(card) => Ok(Reply::View(words::card_view(&head, card, marks))),
                    None => Err(Refusal::Rule(words::no_card(&given, id))),
                },
            }
        }
        Request::Write(write) => {
            let folder = boards.join(&me.root);
            let agents = members(host, &me);
            let clock = || host.now();
            let stamp = || words::stamp(host.now(), |at| host.offset(at));
            store::write(&folder, &write.board, &me.session, &clock, |read, next| {
                apply::apply(read, &me, &agents, write, &stamp(), next)
            })
            .map(Reply::Line)
        }
    }
}

#[cfg(test)]
mod tests;
