use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use serde::Deserialize;

use super::ParsingError;
use super::scope::Scope;
use super::syntax_definition::{Context, ContextId};

#[derive(Debug, Default, Deserialize)]
pub struct SyntaxSet {
    syntaxes: Vec<SyntaxReference>,
    #[allow(dead_code, reason = "read only so the dump's layout deserialises")]
    path_syntaxes: Vec<(String, usize)>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SyntaxReference {
    pub name: String,
    pub file_extensions: Vec<String>,
    pub scope: Scope,
    pub first_line_match: Option<String>,
    pub hidden: bool,
    pub variables: HashMap<String, String>,
    #[serde(skip)]
    lazy_contexts: OnceLock<Option<LazyContexts>>,
    serialized_lazy_contexts: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize)]
struct LazyContexts {
    context_ids: HashMap<String, ContextId>,
    contexts: Vec<Context>,
}

impl SyntaxSet {
    pub fn syntaxes(&self) -> &[SyntaxReference] {
        &self.syntaxes
    }

    pub fn find_syntax_by_name<'a>(&'a self, name: &str) -> Option<&'a SyntaxReference> {
        self.syntaxes.iter().rev().find(|s| s.name == name)
    }

    pub fn index_of(&self, syntax: &SyntaxReference) -> Option<usize> {
        self.syntaxes.iter().position(|s| std::ptr::eq(s, syntax))
    }

    #[inline(always)]
    pub fn get_context(&self, context_id: &ContextId) -> Result<&Context, ParsingError> {
        self.syntaxes
            .get(context_id.syntax_index)
            .and_then(SyntaxReference::contexts)
            .and_then(|contexts| contexts.get(context_id.context_index))
            .ok_or(ParsingError::MissingContext(*context_id))
    }

    pub fn reachable(&self, roots: &[usize]) -> BTreeSet<usize> {
        let mut seen: BTreeSet<usize> = roots.iter().copied().collect();
        let mut queue: Vec<usize> = seen.iter().copied().collect();
        while let Some(index) = queue.pop() {
            let contexts = self
                .syntaxes
                .get(index)
                .and_then(SyntaxReference::contexts)
                .unwrap_or_default();
            for context in contexts {
                for id in context.references() {
                    if seen.insert(id.syntax_index) {
                        queue.push(id.syntax_index);
                    }
                }
            }
        }
        seen
    }
}

impl SyntaxReference {
    pub fn context_ids(&self) -> Option<&HashMap<String, ContextId>> {
        self.lazy_contexts().map(|l| &l.context_ids)
    }

    fn contexts(&self) -> Option<&[Context]> {
        self.lazy_contexts().map(|l| l.contexts.as_slice())
    }

    fn lazy_contexts(&self) -> Option<&LazyContexts> {
        self.lazy_contexts
            .get_or_init(|| {
                crate::dumps::from_compressed_data(self.serialized_lazy_contexts.as_slice()).ok()
            })
            .as_ref()
    }
}
