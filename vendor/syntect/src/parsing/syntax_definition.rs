use regex_syntax::escape;
use serde::Deserialize;

use super::ParsingError;
use super::regex::{Regex, Region};
use super::scope::{ClearAmount, Scope};
use super::syntax_set::SyntaxSet;

pub type CaptureMapping = Vec<(usize, Vec<Scope>)>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord, Deserialize)]
pub struct ContextId {
    pub(crate) syntax_index: usize,
    pub(crate) context_index: usize,
}

impl ContextId {
    pub fn syntax_index(&self) -> usize {
        self.syntax_index
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct Context {
    pub meta_scope: Vec<Scope>,
    pub meta_content_scope: Vec<Scope>,
    pub meta_include_prototype: bool,
    pub clear_scopes: Option<ClearAmount>,
    pub prototype: Option<ContextId>,
    pub uses_backrefs: bool,
    pub patterns: Vec<Pattern>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "upstream's layout; most patterns are matches, so boxing would allocate for each"
)]
pub enum Pattern {
    Match(MatchPattern),
    Include(ContextReference),
}

#[derive(Debug)]
pub struct MatchIter<'a> {
    syntax_set: &'a SyntaxSet,
    ctx_stack: Vec<&'a Context>,
    index_stack: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct MatchPattern {
    pub has_captures: bool,
    pub regex: Regex,
    pub scope: Vec<Scope>,
    pub captures: Option<CaptureMapping>,
    pub operation: MatchOperation,
    pub with_prototype: Option<ContextReference>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[non_exhaustive]
pub enum ContextReference {
    #[non_exhaustive]
    Named(String),
    #[non_exhaustive]
    ByScope {
        scope: Scope,
        sub_context: Option<String>,
        with_escape: bool,
    },
    #[non_exhaustive]
    File {
        name: String,
        sub_context: Option<String>,
        with_escape: bool,
    },
    #[non_exhaustive]
    Inline(String),
    #[non_exhaustive]
    Direct(ContextId),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub enum MatchOperation {
    Push(Vec<ContextReference>),
    Set(Vec<ContextReference>),
    Pop,
    None,
}

impl<'a> Iterator for MatchIter<'a> {
    type Item = (&'a Context, usize);

    fn next(&mut self) -> Option<(&'a Context, usize)> {
        loop {
            let context = *self.ctx_stack.last()?;
            let index = self.index_stack.last_mut()?;
            let current = *index;
            *index += 1;
            match context.patterns.get(current) {
                Some(Pattern::Match(_)) => return Some((context, current)),
                Some(Pattern::Include(ContextReference::Direct(context_id))) => {
                    if let Ok(included) = self.syntax_set.get_context(context_id) {
                        self.ctx_stack.push(included);
                        self.index_stack.push(0);
                    }
                }
                Some(Pattern::Include(_)) => {}
                None => {
                    self.ctx_stack.pop();
                    self.index_stack.pop();
                }
            }
        }
    }
}

pub fn context_iter<'a>(syntax_set: &'a SyntaxSet, context: &'a Context) -> MatchIter<'a> {
    MatchIter {
        syntax_set,
        ctx_stack: vec![context],
        index_stack: vec![0],
    }
}

impl Context {
    pub fn match_at(&self, index: usize) -> Result<&MatchPattern, ParsingError> {
        match self.patterns.get(index) {
            Some(Pattern::Match(match_pat)) => Ok(match_pat),
            _ => Err(ParsingError::BadMatchIndex(index)),
        }
    }

    pub(crate) fn references(&self) -> impl Iterator<Item = &ContextId> {
        let linked = self.patterns.iter().flat_map(|pattern| {
            let refs: Vec<&ContextReference> = match pattern {
                Pattern::Include(r) => vec![r],
                Pattern::Match(m) => {
                    let mut refs: Vec<&ContextReference> = match &m.operation {
                        MatchOperation::Push(v) | MatchOperation::Set(v) => v.iter().collect(),
                        MatchOperation::Pop | MatchOperation::None => vec![],
                    };
                    refs.extend(&m.with_prototype);
                    refs
                }
            };
            refs.into_iter().filter_map(|r| match r {
                ContextReference::Direct(id) => Some(id),
                _ => None,
            })
        });
        self.prototype.iter().chain(linked)
    }
}

impl ContextReference {
    pub fn resolve<'a>(&self, syntax_set: &'a SyntaxSet) -> Result<&'a Context, ParsingError> {
        match *self {
            ContextReference::Direct(ref context_id) => syntax_set.get_context(context_id),
            _ => Err(ParsingError::UnresolvedContextReference(self.clone())),
        }
    }

    pub fn id(&self) -> Result<ContextId, ParsingError> {
        match *self {
            ContextReference::Direct(ref context_id) => Ok(*context_id),
            _ => Err(ParsingError::UnresolvedContextReference(self.clone())),
        }
    }
}

pub(crate) fn substitute_backrefs_in_regex<F>(regex_str: &str, substituter: F) -> String
where
    F: Fn(usize) -> Option<String>,
{
    let mut reg_str = String::with_capacity(regex_str.len());
    let mut last_was_escape = false;
    for c in regex_str.chars() {
        match (last_was_escape, c.to_digit(10)) {
            (true, Some(digit)) => {
                if let Some(sub) = substituter(digit as usize) {
                    reg_str.push_str(&sub);
                }
            }
            (true, None) => {
                reg_str.push('\\');
                reg_str.push(c);
            }
            (false, _) if c != '\\' => reg_str.push(c),
            (false, _) => {}
        }
        last_was_escape = c == '\\' && !last_was_escape;
    }
    if last_was_escape {
        reg_str.push('\\');
    }
    reg_str
}

impl MatchPattern {
    pub fn regex_with_refs(&self, region: &Region, text: &str) -> Regex {
        let new_regex = substitute_backrefs_in_regex(self.regex.regex_str(), |i| {
            region
                .pos(i)
                .and_then(|(start, end)| text.get(start..end))
                .map(escape)
        });
        Regex::new(new_regex)
    }

    pub fn regex(&self) -> &Regex {
        &self.regex
    }
}
