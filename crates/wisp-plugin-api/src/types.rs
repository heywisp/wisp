#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalKind {
    Interrupt,
    Kill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackgroundReason {
    Started,
    Quiet,
    MovedByUser,
    StillRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Run,
    Read,
    Write,
    Edit,
    Search,
    Fetch,
    Input,
    Wait,
    Attach,
    Show,
    Signal(SignalKind),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verbs {
    pub doing: String,
    pub done: String,
    pub failed: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputKind {
    Text,
    Code(String),
    Diff,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    pub path: Option<String>,
    pub old_path: Option<String>,
    pub language: Option<String>,
    pub hunks: String,
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub title: Option<String>,
    pub files: Vec<DiffFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NamedKey {
    Enter,
    Tab,
    Space,
    Esc,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

impl NamedKey {
    pub const ALL: [Self; 14] = [
        Self::Enter,
        Self::Tab,
        Self::Space,
        Self::Esc,
        Self::Backspace,
        Self::Delete,
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::Home,
        Self::End,
        Self::PageUp,
        Self::PageDown,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Enter => "Enter",
            Self::Tab => "Tab",
            Self::Space => "Space",
            Self::Esc => "Esc",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Home => "Home",
            Self::End => "End",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Named(NamedKey),
    Function(u8),
    Ctrl(char),
    Eof,
}

impl Key {
    pub fn name(&self) -> String {
        match self {
            Self::Named(key) => key.name().to_owned(),
            Self::Function(n) => format!("F{n}"),
            Self::Ctrl(letter) => format!("C-{letter}"),
            Self::Eof => "EOF".to_owned(),
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        if name == "EOF" {
            return Some(Self::Eof);
        }
        if let Some(key) = NamedKey::ALL.into_iter().find(|k| k.name() == name) {
            return Some(Self::Named(key));
        }
        if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok())
            && (1..=12).contains(&n)
            && !name[1..].starts_with('0')
        {
            return Some(Self::Function(n));
        }
        let letter = name.strip_prefix("C-")?;
        let mut chars = letter.chars();
        let c = chars.next()?;
        if chars.next().is_some() || !c.is_ascii_alphabetic() {
            return None;
        }
        Some(Self::Ctrl(c.to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadKind {
    New,
    Tail,
    Lines,
    Search,
    Screen,
}

impl ReadKind {
    pub const ALL: [Self; 5] = [
        Self::New,
        Self::Tail,
        Self::Lines,
        Self::Search,
        Self::Screen,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Tail => "tail",
            Self::Lines => "lines",
            Self::Search => "search",
            Self::Screen => "screen",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessView {
    pub id: Option<u32>,
    pub text: Option<String>,
    pub keys: Vec<Key>,
    pub signal: Option<SignalKind>,
    pub read: Option<ReadKind>,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeView {
    pub language: Option<String>,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Diff(DiffView),
    Process(ProcessView),
    Code(CodeView),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub action: Action,
    pub verbs: Option<Verbs>,
    pub target: String,
    pub output: OutputKind,
    pub view: Option<View>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Matches {
    pub n: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diffstat {
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub sent: u64,
    pub total: u64,
    pub more_than: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: crate::index::Kind,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fact {
    Exit(i32),
    Lines(u64),
    Bytes(u64),
    Matches(Matches),
    Diffstat(Diffstat),
    Process(u32),
    Media(String),
    Part(Part),
    Tokens(u64),
    Hits(Vec<Hit>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(i32),
    Signal(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Completed {
    pub is_error: bool,
    pub exit: Option<ExitStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub before_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed(Completed),
    TimedOut(u64),
    Cancelled,
    InvalidInput(String),
    Failed(Failure),
    Backgrounded(BackgroundReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceKind {
    Pages,
    Seconds,
    Sheet,
}

impl SliceKind {
    pub const ALL: &'static [Self] = &[Self::Pages, Self::Seconds, Self::Sheet];

    pub fn name(&self) -> &'static str {
        match self {
            Self::Pages => "pages",
            Self::Seconds => "seconds",
            Self::Sheet => "sheet",
        }
    }

    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageSlice {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondSlice {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSlice {
    Pages(PageSlice),
    Seconds(SecondSlice),
    Sheet(u32),
}

impl FileSlice {
    pub fn kind(&self) -> SliceKind {
        match self {
            Self::Pages(_) => SliceKind::Pages,
            Self::Seconds(_) => SliceKind::Seconds,
            Self::Sheet(_) => SliceKind::Sheet,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub slice: Option<FileSlice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typed {
    pub echoed: bool,
    pub unsent: bool,
    pub signalled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRun {
    pub outcome: Outcome,
    pub facts: Vec<Fact>,
    pub cwd: Option<String>,
    pub trailer: Option<String>,
    pub result: Option<String>,
    pub attachments: Vec<Attachment>,
    pub typed: Option<Typed>,
}

impl ToolRun {
    pub fn new(outcome: Outcome) -> Self {
        Self {
            outcome,
            facts: Vec::new(),
            cwd: None,
            trailer: None,
            result: None,
            attachments: Vec::new(),
            typed: None,
        }
    }

    pub fn failed_to_start(message: impl Into<String>) -> Self {
        Self::new(Outcome::Failed(Failure {
            message: message.into(),
            before_start: true,
        }))
    }

    pub fn completed() -> Self {
        Self::new(Outcome::Completed(Completed {
            is_error: false,
            exit: None,
        }))
    }

    #[must_use]
    pub fn with_result(self, text: impl Into<String>) -> Self {
        Self {
            result: Some(text.into()),
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub header: String,
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub chosen: Vec<u32>,
    pub own: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskReply {
    Answered(Vec<Answer>),
    Attached(bool),
    Declined,
    ProcessEnded(bool),
    MessageInstead,
    Unavailable,
    Stopped,
}
