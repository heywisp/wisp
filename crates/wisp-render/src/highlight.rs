mod cache;
mod diff;
mod scopes;

use std::collections::BTreeSet;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use syntect::dumps::from_uncompressed_data;
use syntect::parsing::{
    LineLimits, ParseState, ParsingError, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::markdown::{
    CodeKey, CodeLines, CodeStyle, DiffKind, FenceAt, Revision, diff_hint, diff_kind,
};
use crate::styled::{Look, StyledSpan};
use crate::text::spans_text;
use crate::theme::Token;

use diff::DiffFull;

pub use cache::{
    CachedCode, CachedRows, CodeUpdate, CodeWorker, CutFeed, FenceDelta, FenceSources, KeptCode,
    KeptStyle, SpanCache,
};
pub use scopes::{Rule, SCOPES, ScopeRules};

pub const MAX_LINE_BYTES: usize = 2 * 1024;
pub const MAX_LINES: usize = 10_000;
pub const MAX_BLOCK_BYTES: usize = 128 * 1024;
pub const MAX_INFO_BYTES: usize = 64;

const GRAMMARS: &[u8] = include_bytes!("../assets/grammars.bin");

pub const LANGS: [(&str, &str); 84] = [
    ("bash", "Bourne Again Shell (bash)"),
    ("c", "C"),
    ("c#", "C#"),
    ("c++", "C++"),
    ("cc", "C++"),
    ("cjs", "JavaScript (Babel)"),
    ("containerfile", "Dockerfile"),
    ("cpp", "C++"),
    ("cs", "C#"),
    ("csharp", "C#"),
    ("css", "CSS"),
    ("cts", "TypeScript"),
    ("cxx", "C++"),
    ("docker", "Dockerfile"),
    ("dockerfile", "Dockerfile"),
    ("dot", "Graphviz (DOT)"),
    ("elixir", "Elixir"),
    ("ex", "Elixir"),
    ("exs", "Elixir"),
    ("go", "Go"),
    ("golang", "Go"),
    ("graphql", "GraphQL"),
    ("h", "C"),
    ("haskell", "Haskell"),
    ("hpp", "C++"),
    ("hs", "Haskell"),
    ("htm", "HTML"),
    ("html", "HTML"),
    ("java", "Java"),
    ("javascript", "JavaScript (Babel)"),
    ("js", "JavaScript (Babel)"),
    ("json", "JSON"),
    ("json5", "JSON"),
    ("jsonc", "JSON"),
    ("jsx", "JavaScript (Babel)"),
    ("kotlin", "Kotlin"),
    ("kt", "Kotlin"),
    ("kts", "Kotlin"),
    ("lua", "Lua"),
    ("make", "Makefile"),
    ("makefile", "Makefile"),
    ("markdown", "Markdown"),
    ("md", "Markdown"),
    ("mjs", "JavaScript (Babel)"),
    ("mk", "Makefile"),
    ("mts", "TypeScript"),
    ("mysql", "SQL"),
    ("nix", "Nix"),
    ("objc", "Objective-C"),
    ("objective-c", "Objective-C"),
    ("php", "PHP"),
    ("postgresql", "SQL"),
    ("powershell", "PowerShell"),
    ("ps1", "PowerShell"),
    ("psql", "SQL"),
    ("pwsh", "PowerShell"),
    ("py", "Python"),
    ("py3", "Python"),
    ("python", "Python"),
    ("python3", "Python"),
    ("r", "R"),
    ("rb", "Ruby"),
    ("rs", "Rust"),
    ("ruby", "Ruby"),
    ("rust", "Rust"),
    ("sass", "Sass"),
    ("sbt", "Scala"),
    ("sc", "Scala"),
    ("scala", "Scala"),
    ("scss", "SCSS"),
    ("sh", "Bourne Again Shell (bash)"),
    ("shell", "Bourne Again Shell (bash)"),
    ("sql", "SQL"),
    ("sqlite", "SQL"),
    ("swift", "Swift"),
    ("toml", "TOML"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScriptReact"),
    ("typescript", "TypeScript"),
    ("xml", "XML"),
    ("yaml", "YAML"),
    ("yml", "YAML"),
    ("zig", "Zig"),
    ("zsh", "Bourne Again Shell (bash)"),
];

pub const EMBEDDED: [&str; 16] = [
    "Graphviz (DOT)",
    "Javadoc",
    "Objective-C",
    "Objective-C++",
    "PHP Source",
    "XML",
    "GraphQL",
    "Puppet",
    "Sass",
    "Shell-Unix-Generic",
    "commands-builtin-shell-bash",
    "Regular Expression",
    "Regular Expressions (Javascript)",
    "Regular Expressions (PHP)",
    "Regular Expressions (Python)",
    "Regular Expressions (Elixir)",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrammarError {
    Corrupt,
    Missing(&'static str),
}

impl fmt::Display for GrammarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GrammarError::Corrupt => f.write_str("the bundled grammar set could not be read"),
            GrammarError::Missing(name) => {
                write!(f, "the bundled grammar set has no {name} grammar")
            }
        }
    }
}

impl std::error::Error for GrammarError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    pub line_bytes: usize,
    pub lines: usize,
    pub block_bytes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            line_bytes: MAX_LINE_BYTES,
            lines: MAX_LINES,
            block_bytes: MAX_BLOCK_BYTES,
        }
    }
}

impl Limits {
    fn params(self) -> [u64; 3] {
        [self.line_bytes, self.lines, self.block_bytes]
            .map(|v| u64::try_from(v).unwrap_or(u64::MAX))
    }
}

pub struct Grammars {
    syntaxes: SyntaxSet,
    rules: ScopeRules,
    langs: Vec<(&'static str, usize)>,
}

impl fmt::Debug for Grammars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Grammars")
            .field("syntaxes", &self.syntaxes.syntaxes().len())
            .field("langs", &self.langs.len())
            .finish()
    }
}

impl Grammars {
    pub fn load() -> Result<Grammars, GrammarError> {
        let syntaxes: SyntaxSet =
            from_uncompressed_data(GRAMMARS).map_err(|_| GrammarError::Corrupt)?;
        let index = |name: &'static str| {
            syntaxes
                .syntaxes()
                .iter()
                .position(|s| s.name == name)
                .ok_or(GrammarError::Missing(name))
        };
        let langs = LANGS
            .iter()
            .map(|(token, name)| Ok((*token, index(name)?)))
            .collect::<Result<Vec<_>, GrammarError>>()?;
        for name in EMBEDDED {
            index(name)?;
        }
        Ok(Grammars {
            syntaxes,
            rules: ScopeRules::new(),
            langs,
        })
    }

    pub fn syntax(&self, lang: &str) -> Option<&SyntaxReference> {
        self.syntax_index(lang)
            .and_then(|index| self.syntaxes.syntaxes().get(index))
    }

    fn syntax_index(&self, lang: &str) -> Option<usize> {
        let lang = lang.trim();
        if lang.is_empty() || lang.len() > MAX_INFO_BYTES {
            return None;
        }
        let lang = lang.to_lowercase();
        self.langs
            .iter()
            .find(|(token, _)| *token == lang)
            .map(|(_, index)| *index)
    }

    fn syntax_at(&self, index: usize) -> Option<&SyntaxReference> {
        self.syntaxes.syntaxes().get(index)
    }

    pub fn allowed(&self) -> BTreeSet<&str> {
        self.langs
            .iter()
            .filter_map(|(_, i)| self.syntaxes.syntaxes().get(*i))
            .map(|s| s.name.as_str())
            .collect()
    }

    pub fn reachable(&self) -> BTreeSet<&str> {
        let roots: Vec<usize> = self.langs.iter().map(|(_, i)| *i).collect();
        self.syntaxes
            .reachable(&roots)
            .into_iter()
            .filter_map(|i| self.syntaxes.syntaxes().get(i))
            .map(|s| s.name.as_str())
            .collect()
    }

    pub fn syntax_by_name(&self, name: &str) -> Option<&SyntaxReference> {
        self.syntaxes.find_syntax_by_name(name)
    }

    pub fn syntax_set(&self) -> &SyntaxSet {
        &self.syntaxes
    }

    fn look(&self, stack: &ScopeStack) -> Look {
        self.token(stack).map_or(Look::default(), Look::fg)
    }

    pub fn token(&self, stack: &ScopeStack) -> Option<Token> {
        self.rules.token(stack.as_slice())
    }
}

#[derive(Debug)]
pub struct Highlighter {
    grammars: Option<Arc<Grammars>>,
    limits: Limits,
}

impl Default for Highlighter {
    fn default() -> Highlighter {
        Highlighter::pending()
    }
}

impl Highlighter {
    pub fn pending() -> Highlighter {
        Highlighter {
            grammars: None,
            limits: Limits::default(),
        }
    }

    pub fn ready(grammars: impl Into<Arc<Grammars>>) -> Highlighter {
        Highlighter {
            grammars: Some(grammars.into()),
            limits: Limits::default(),
        }
    }

    pub fn with_limits(self, limits: Limits) -> Highlighter {
        Highlighter { limits, ..self }
    }

    pub fn install(&mut self, grammars: impl Into<Arc<Grammars>>) {
        self.grammars = Some(grammars.into());
    }

    pub fn is_ready(&self) -> bool {
        self.grammars.is_some()
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn grammars(&self) -> Option<&Arc<Grammars>> {
        self.grammars.as_ref()
    }

    pub fn cursor(&self, lang: Option<&str>) -> CodeCursor {
        CodeCursor::new(self.grammars.as_ref(), lang, self.limits)
    }
}

impl CodeStyle for Highlighter {
    fn key(&self) -> CodeKey {
        CodeKey {
            name: "highlight",
            generation: u64::from(self.is_ready()),
            params: self.limits.params(),
        }
    }

    fn start<'s>(&'s self, lang: Option<&str>, _at: FenceAt) -> Box<dyn CodeLines + 's> {
        Box::new(self.cursor(lang))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stop {
    LineTooLong,
    TooManyLines,
    BlockTooBig,
    Budget,
    Cancelled,
    Parse,
}

#[derive(Clone, Debug)]
enum Mode {
    Plain,
    Diff(wisp_diff::Lines),
    DiffFull(Box<DiffFull>),
    Syntax {
        grammars: Arc<Grammars>,
        state: ParseState,
        stack: ScopeStack,
    },
}

#[derive(Clone, Debug)]
pub struct CodeCursor {
    mode: Mode,
    limits: Limits,
    lines: usize,
    bytes: usize,
    stop: Option<Stop>,
}

static NEVER: AtomicBool = AtomicBool::new(false);

impl CodeCursor {
    pub fn new(grammars: Option<&Arc<Grammars>>, lang: Option<&str>, limits: Limits) -> CodeCursor {
        let mut cursor = CodeCursor {
            mode: Mode::Plain,
            limits,
            lines: 0,
            bytes: 0,
            stop: None,
        };
        if let Some(kind) = diff_kind(lang) {
            let lines = match kind {
                DiffKind::Full => wisp_diff::Lines::full(),
                DiffKind::Hunks => wisp_diff::Lines::hunks_only(),
            };
            cursor.mode = match grammars {
                Some(grammars) => {
                    Mode::DiffFull(Box::new(DiffFull::new(grammars, lines, diff_hint(lang))))
                }
                None => Mode::Diff(lines),
            };
            return cursor;
        }
        let found = grammars
            .zip(lang)
            .and_then(|(g, lang)| g.syntax(lang).map(|s| (g, s)));
        if let Some((grammars, syntax)) = found {
            cursor.start_syntax(grammars, syntax);
        }
        cursor
    }

    pub fn for_syntax(grammars: &Arc<Grammars>, name: &str, limits: Limits) -> CodeCursor {
        let mut cursor = CodeCursor::new(None, None, limits);
        if let Some(syntax) = grammars.syntax_by_name(name) {
            cursor.start_syntax(grammars, syntax);
        }
        cursor
    }

    fn start_syntax(&mut self, grammars: &Arc<Grammars>, syntax: &SyntaxReference) {
        match ParseState::new(syntax) {
            Ok(state) => {
                self.mode = Mode::Syntax {
                    grammars: Arc::clone(grammars),
                    state,
                    stack: ScopeStack::new(),
                }
            }
            Err(_) => self.stop = Some(Stop::Parse),
        }
    }

    pub fn is_highlighting(&self) -> bool {
        matches!(self.mode, Mode::Syntax { .. }) && self.stop.is_none()
    }

    pub fn stop(&self) -> Option<Stop> {
        self.stop
    }

    pub fn lines(&self) -> usize {
        self.lines
    }

    pub fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        self.line_with_cancel(line, &NEVER)
    }

    pub fn line_with_cancel(&mut self, line: &str, cancel: &AtomicBool) -> Vec<StyledSpan> {
        self.lines = self.lines.saturating_add(1);
        self.bytes = self.bytes.saturating_add(line.len() + 1);
        let over = if line.len() > self.limits.line_bytes {
            Some(Stop::LineTooLong)
        } else if self.lines > self.limits.lines {
            Some(Stop::TooManyLines)
        } else if self.bytes > self.limits.block_bytes {
            Some(Stop::BlockTooBig)
        } else {
            None
        };
        match &mut self.mode {
            Mode::Plain => plain(line),
            Mode::Diff(lines) => diff::structure(lines.push(line).kind, line),
            Mode::DiffFull(full) => {
                if self.stop.is_none() {
                    self.stop = over;
                }
                let (spans, stop) = full.line(line, self.stop.is_none(), cancel);
                if self.stop.is_none() {
                    self.stop = stop;
                }
                spans
            }
            Mode::Syntax { .. } if self.stop.is_some() => plain(line),
            Mode::Syntax { .. } if over.is_some() => {
                self.stop = over;
                plain(line)
            }
            Mode::Syntax {
                grammars,
                state,
                stack,
            } => match highlight(grammars, state, stack, line, cancel) {
                Ok(spans) => spans,
                Err(stop) => {
                    self.stop = Some(stop);
                    plain(line)
                }
            },
        }
    }
}

impl CodeCursor {
    fn forget_run(&mut self) {
        if let Mode::DiffFull(full) = &mut self.mode {
            full.forget_run();
        }
    }

    pub fn revise(&mut self) -> Option<Revision> {
        match &mut self.mode {
            Mode::DiffFull(full) => full.revise(),
            _ => None,
        }
    }
}

impl CodeLines for CodeCursor {
    fn line(&mut self, line: &str) -> Vec<StyledSpan> {
        CodeCursor::line(self, line)
    }

    fn revise(&mut self) -> Option<Revision> {
        CodeCursor::revise(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLine {
    pub text: String,
    pub spans: Vec<StyledSpan>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedError {
    Diverged { at: usize },
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeedError::Diverged { at } => {
                write!(f, "the code no longer extends what was fed (byte {at})")
            }
        }
    }
}

impl std::error::Error for FeedError {}

#[derive(Clone, Debug)]
pub struct HighlightedCode {
    start: CodeCursor,
    cursor: CodeCursor,
    lines: Vec<CodeLine>,
    fed: String,
    revised: Option<usize>,
}

impl HighlightedCode {
    pub fn new(cursor: CodeCursor) -> HighlightedCode {
        HighlightedCode {
            start: cursor.clone(),
            cursor,
            lines: vec![],
            fed: String::new(),
            revised: None,
        }
    }

    pub fn feed(&mut self, text: &str) -> Result<Range<usize>, FeedError> {
        self.feed_with_cancel(text, &NEVER)
    }

    pub fn feed_with_cancel(
        &mut self,
        text: &str,
        cancel: &AtomicBool,
    ) -> Result<Range<usize>, FeedError> {
        let fresh = match text.strip_prefix(self.fed.as_str()) {
            Some(fresh) => fresh,
            None => {
                let at = text
                    .bytes()
                    .zip(self.fed.bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                return Err(FeedError::Diverged { at });
            }
        };
        let first = self.lines.len();
        let mut start = 0;
        while let Some(rest) = fresh.get(start..) {
            let Some(newline) = rest.find('\n') else {
                break;
            };
            self.push(rest.get(..newline).unwrap_or(""), cancel);
            start += newline + 1;
        }
        let first = self.take_revised().map_or(first, |r| r.min(first));
        Ok(first..self.lines.len())
    }

    pub fn push(&mut self, line: &str, cancel: &AtomicBool) {
        let spans = self.cursor.line_with_cancel(line, cancel);
        self.lines.push(CodeLine {
            text: line.to_owned(),
            spans,
        });
        self.fed.push_str(line);
        self.fed.push('\n');
        if let Some(revision) = self.cursor.revise() {
            self.mark_revised(revision.from);
            revision.apply(&mut self.lines, |line, spans| line.spans = spans);
        }
    }

    fn mark_revised(&mut self, from: usize) {
        self.revised = Some(self.revised.map_or(from, |r| r.min(from)));
    }

    pub fn take_revised(&mut self) -> Option<usize> {
        self.revised.take()
    }

    pub fn rewind(&mut self, keep: usize, cancel: &AtomicBool) {
        let kept: Vec<CodeLine> = self.lines.iter().take(keep).cloned().collect();
        let revised = self.revised;
        self.reset();
        for line in &kept {
            self.push(&line.text, cancel);
        }
        self.revised = revised;
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            for (line, was) in self.lines.iter_mut().zip(kept) {
                *line = was;
            }
            self.cursor.forget_run();
            return;
        }
        if let Some(changed) = kept
            .iter()
            .zip(&self.lines)
            .position(|(was, now)| was.spans != now.spans)
        {
            self.mark_revised(changed);
        }
    }

    pub fn reset(&mut self) {
        self.cursor = self.start.clone();
        self.lines.clear();
        self.fed.clear();
        self.revised = None;
    }

    pub fn lines(&self) -> &[CodeLine] {
        &self.lines
    }

    pub fn cursor(&self) -> &CodeCursor {
        &self.cursor
    }
}

fn plain(line: &str) -> Vec<StyledSpan> {
    if line.is_empty() {
        vec![]
    } else {
        vec![StyledSpan::plain(line)]
    }
}

fn highlight(
    grammars: &Grammars,
    state: &mut ParseState,
    stack: &mut ScopeStack,
    line: &str,
    cancel: &AtomicBool,
) -> Result<Vec<StyledSpan>, Stop> {
    let mut source = String::with_capacity(line.len() + 1);
    source.push_str(line);
    source.push('\n');
    let limits = LineLimits::DEFAULT.with_cancel(cancel);
    let ops = state
        .parse_line_limited(&source, &grammars.syntaxes, &limits)
        .map_err(|e| match e {
            ParsingError::BudgetExceeded => Stop::Budget,
            ParsingError::Cancelled => Stop::Cancelled,
            _ => Stop::Parse,
        })?;
    paint(grammars, stack, line, &ops).ok_or(Stop::Parse)
}

fn paint(
    grammars: &Grammars,
    stack: &mut ScopeStack,
    line: &str,
    ops: &[(usize, ScopeStackOp)],
) -> Option<Vec<StyledSpan>> {
    let bounds: Vec<usize> = line
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([line.len()])
        .collect();
    let snap = |p: usize| {
        let p = p.min(line.len());
        match bounds.binary_search(&p) {
            Ok(_) => p,
            Err(i) => bounds.get(i.wrapping_sub(1)).copied().unwrap_or(0),
        }
    };
    let look = |stack: &ScopeStack| grammars.look(stack);
    let mut out: Vec<StyledSpan> = vec![];
    let mut at = 0;
    for (pos, op) in ops {
        let to = snap(*pos);
        if to > at {
            push(&mut out, line.get(at..to)?, look(stack));
            at = to;
        }
        stack.apply(op).ok()?;
    }
    if at < line.len() {
        push(&mut out, line.get(at..)?, look(stack));
    }
    let whole = spans_text(&out) == line;
    whole.then_some(out)
}

fn push(out: &mut Vec<StyledSpan>, text: &str, look: Look) {
    if text.is_empty() {
        return;
    }
    match out.last_mut() {
        Some(last) if last.look == look => last.text.push_str(text),
        _ => out.push(StyledSpan::new(text, look)),
    }
}

#[cfg(test)]
mod tests;
