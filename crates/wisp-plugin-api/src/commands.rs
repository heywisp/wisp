use crate::slots::TextSpan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReply {
    None,
    Note(String),
    Send(String),
    Lines(Vec<CommandLine>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLine {
    Row(CommandRow),
    Text(Vec<TextSpan>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRow {
    pub left: Vec<TextSpan>,
    pub right: Vec<TextSpan>,
}
