#![allow(
    clippy::mutable_key_type,
    reason = "the search cache is keyed by pattern address, as upstream"
)]

use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::sync::atomic::{AtomicBool, Ordering};

use fnv::FnvHasher;

use super::regex::Region;
use super::scope::ScopeStackOp;
use super::syntax_definition::{
    Context, ContextId, ContextReference, MatchOperation, MatchPattern, context_iter,
};
use super::syntax_set::{SyntaxReference, SyntaxSet};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ParsingError {
    #[error("Somehow main context was popped from the stack")]
    MissingMainContext,
    #[error("Missing context with ID '{0:?}'")]
    MissingContext(ContextId),
    #[error("Bad index to match_at: {0}")]
    BadMatchIndex(usize),
    #[error("Tried to use a ContextReference that has not bee resolved yet: {0:?}")]
    UnresolvedContextReference(ContextReference),
    #[error("The line's parsing budget was exceeded")]
    BudgetExceeded,
    #[error("Parsing was cancelled")]
    Cancelled,
}

#[derive(Clone, Copy, Debug)]
pub struct LineLimits<'c> {
    pub iterations_per_byte: u64,
    pub iterations_base: u64,
    pub searches_per_byte: u64,
    pub searches_base: u64,
    pub stall: u32,
    pub cancel: Option<&'c AtomicBool>,
}

impl LineLimits<'static> {
    pub const DEFAULT: LineLimits<'static> = LineLimits {
        iterations_per_byte: 8,
        iterations_base: 64,
        searches_per_byte: 32,
        searches_base: 256,
        stall: 64,
        cancel: None,
    };
}

impl Default for LineLimits<'static> {
    fn default() -> LineLimits<'static> {
        LineLimits::DEFAULT
    }
}

impl<'c> LineLimits<'c> {
    pub fn with_cancel<'n>(self, cancel: &'n AtomicBool) -> LineLimits<'n> {
        LineLimits {
            iterations_per_byte: self.iterations_per_byte,
            iterations_base: self.iterations_base,
            searches_per_byte: self.searches_per_byte,
            searches_base: self.searches_base,
            stall: self.stall,
            cancel: Some(cancel),
        }
    }

    pub fn iterations(&self, line_len: usize) -> u64 {
        budget(self.iterations_per_byte, self.iterations_base, line_len)
    }

    pub fn searches(&self, line_len: usize) -> u64 {
        budget(self.searches_per_byte, self.searches_base, line_len)
    }
}

fn budget(per_byte: u64, base: u64, line_len: usize) -> u64 {
    per_byte
        .saturating_mul(
            u64::try_from(line_len)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .saturating_add(base)
}

struct Budget<'c> {
    searches: u64,
    search_limit: u64,
    cancel: Option<&'c AtomicBool>,
}

impl Budget<'_> {
    fn check_cancel(&self) -> Result<(), ParsingError> {
        match self.cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(ParsingError::Cancelled),
            _ => Ok(()),
        }
    }

    fn spend_search(&mut self) -> Result<(), ParsingError> {
        self.check_cancel()?;
        self.searches += 1;
        if self.searches > self.search_limit {
            return Err(ParsingError::BudgetExceeded);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ParseState {
    stack: Vec<StateLevel>,
    first_line: bool,
    proto_starts: Vec<usize>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct StateLevel {
    context: ContextId,
    prototypes: Vec<ContextId>,
    captures: Option<(Region, String)>,
}

#[derive(Debug)]
struct RegexMatch<'a> {
    regions: Region,
    context: &'a Context,
    pat_index: usize,
    from_with_prototype: bool,
    would_loop: bool,
}

type SearchCache = HashMap<*const MatchPattern, Option<Region>, BuildHasherDefault<FnvHasher>>;

const MAX_STACK_DEPTH: usize = 100;

fn match_bounds(regions: &Region) -> Result<(usize, usize), ParsingError> {
    regions.pos(0).ok_or(ParsingError::BadMatchIndex(0))
}

impl ParseState {
    pub fn new(syntax: &SyntaxReference) -> Result<ParseState, ParsingError> {
        let start = syntax
            .context_ids()
            .and_then(|ids| ids.get("__start"))
            .ok_or(ParsingError::MissingMainContext)?;
        Ok(ParseState {
            stack: vec![StateLevel {
                context: *start,
                prototypes: Vec::new(),
                captures: None,
            }],
            first_line: true,
            proto_starts: Vec::new(),
        })
    }

    pub fn parse_line(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
    ) -> Result<Vec<(usize, ScopeStackOp)>, ParsingError> {
        self.parse_line_limited(line, syntax_set, &LineLimits::DEFAULT)
    }

    pub fn parse_line_limited(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        limits: &LineLimits<'_>,
    ) -> Result<Vec<(usize, ScopeStackOp)>, ParsingError> {
        let mut match_start = 0;
        let mut res = Vec::new();

        if self.first_line {
            let cur_level = self.stack.last().ok_or(ParsingError::MissingMainContext)?;
            let context = syntax_set.get_context(&cur_level.context)?;
            if let Some(scope) = context.meta_content_scope.first() {
                res.push((0, ScopeStackOp::Push(*scope)));
            }
            self.first_line = false;
        }

        let mut regions = Region::new();
        let mut search_cache: SearchCache =
            HashMap::with_capacity_and_hasher(128, BuildHasherDefault::<FnvHasher>::default());
        let mut non_consuming_push_at = (0, 0);
        let mut budget = Budget {
            searches: 0,
            search_limit: limits.searches(line.len()),
            cancel: limits.cancel,
        };
        let iteration_limit = limits.iterations(line.len());
        let mut iterations: u64 = 0;
        let mut stalled_at = usize::MAX;
        let mut stalls: u32 = 0;

        while self.parse_next_token(
            line,
            syntax_set,
            &mut match_start,
            &mut search_cache,
            &mut regions,
            &mut non_consuming_push_at,
            &mut res,
            &mut budget,
        )? {
            budget.check_cancel()?;
            iterations += 1;
            if iterations > iteration_limit {
                return Err(ParsingError::BudgetExceeded);
            }
            if stalled_at != match_start {
                stalled_at = match_start;
                stalls = 0;
                continue;
            }
            stalls += 1;
            if stalls > limits.stall {
                match line
                    .get(match_start..)
                    .and_then(|rest| rest.char_indices().nth(1))
                {
                    Some((i, _)) => match_start += i,
                    None => break,
                }
                stalled_at = usize::MAX;
                stalls = 0;
            }
        }

        Ok(res)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "upstream's signature plus the line budget"
    )]
    fn parse_next_token(
        &mut self,
        line: &str,
        syntax_set: &SyntaxSet,
        start: &mut usize,
        search_cache: &mut SearchCache,
        regions: &mut Region,
        non_consuming_push_at: &mut (usize, usize),
        ops: &mut Vec<(usize, ScopeStackOp)>,
        budget: &mut Budget<'_>,
    ) -> Result<bool, ParsingError> {
        let check_pop_loop = {
            let (pos, stack_depth) = *non_consuming_push_at;
            pos == *start && stack_depth == self.stack.len()
        };

        while self
            .proto_starts
            .last()
            .is_some_and(|start| *start >= self.stack.len())
        {
            self.proto_starts.pop();
        }

        let best_match = self.find_best_match(
            line,
            *start,
            syntax_set,
            search_cache,
            regions,
            check_pop_loop,
            budget,
        )?;

        let Some(reg_match) = best_match else {
            return Ok(false);
        };

        if reg_match.would_loop {
            return match line
                .get(*start..)
                .and_then(|rest| rest.char_indices().nth(1))
            {
                Some((i, _)) => {
                    *start += i;
                    Ok(true)
                }
                None => Ok(false),
            };
        }

        let (_, match_end) = match_bounds(&reg_match.regions)?;

        if match_end <= *start {
            let match_pattern = reg_match.context.match_at(reg_match.pat_index)?;
            if let MatchOperation::Push(_) = match_pattern.operation {
                *non_consuming_push_at = (match_end, self.stack.len() + 1);
            }
        }

        *start = match_end;

        if reg_match.from_with_prototype {
            self.proto_starts.push(self.stack.len());
        }

        let level_context = {
            let level = self.stack.last().ok_or(ParsingError::MissingMainContext)?;
            syntax_set.get_context(&level.context)?
        };
        self.exec_pattern(line, &reg_match, level_context, syntax_set, ops)?;

        Ok(true)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "upstream's signature plus the line budget"
    )]
    fn find_best_match<'a>(
        &self,
        line: &str,
        start: usize,
        syntax_set: &'a SyntaxSet,
        search_cache: &mut SearchCache,
        regions: &mut Region,
        check_pop_loop: bool,
        budget: &mut Budget<'_>,
    ) -> Result<Option<RegexMatch<'a>>, ParsingError> {
        let cur_level = self.stack.last().ok_or(ParsingError::MissingMainContext)?;
        let context = syntax_set.get_context(&cur_level.context)?;
        let prototype = context.prototype.as_ref();

        let context_chain = {
            let proto_start = self.proto_starts.last().copied().unwrap_or(0);
            let with_prototypes = self
                .stack
                .get(proto_start..)
                .unwrap_or_default()
                .iter()
                .flat_map(|lvl| {
                    lvl.prototypes
                        .iter()
                        .map(move |ctx| (true, ctx, lvl.captures.as_ref()))
                });
            let cur_prototype = prototype.into_iter().map(|ctx| (false, ctx, None));
            let cur_context =
                Some((false, &cur_level.context, cur_level.captures.as_ref())).into_iter();
            with_prototypes.chain(cur_prototype).chain(cur_context)
        };

        let mut min_start = usize::MAX;
        let mut best_match: Option<RegexMatch<'_>> = None;
        let mut pop_would_loop = false;

        for (from_with_proto, ctx, captures) in context_chain {
            for (pat_context, pat_index) in context_iter(syntax_set, syntax_set.get_context(ctx)?) {
                let match_pat = pat_context.match_at(pat_index)?;

                let Some(match_region) = self.search(
                    line,
                    start,
                    match_pat,
                    captures,
                    search_cache,
                    regions,
                    budget,
                )?
                else {
                    continue;
                };
                let (match_start, match_end) = match_bounds(&match_region)?;

                if match_start < min_start || (match_start == min_start && pop_would_loop) {
                    min_start = match_start;

                    let consuming = match_end > start;
                    pop_would_loop = check_pop_loop
                        && !consuming
                        && matches!(match_pat.operation, MatchOperation::Pop);

                    let push_too_deep = matches!(match_pat.operation, MatchOperation::Push(_))
                        && self.stack.len() >= MAX_STACK_DEPTH;

                    if push_too_deep {
                        return Ok(None);
                    }

                    best_match = Some(RegexMatch {
                        regions: match_region,
                        context: pat_context,
                        pat_index,
                        from_with_prototype: from_with_proto,
                        would_loop: pop_would_loop,
                    });

                    if match_start == start && !pop_would_loop {
                        return Ok(best_match);
                    }
                }
            }
        }
        Ok(best_match)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "upstream's signature plus the line budget"
    )]
    fn search(
        &self,
        line: &str,
        start: usize,
        match_pat: &MatchPattern,
        captures: Option<&(Region, String)>,
        search_cache: &mut SearchCache,
        regions: &mut Region,
        budget: &mut Budget<'_>,
    ) -> Result<Option<Region>, ParsingError> {
        let match_ptr = match_pat as *const MatchPattern;

        if let Some(maybe_region) = search_cache.get(&match_ptr) {
            match maybe_region {
                None => return Ok(None),
                Some(region) if region.pos(0).is_some_and(|(s, _)| s >= start) => {
                    return Ok(Some(region.clone()));
                }
                Some(_) => {}
            }
        }

        let with_refs;
        let (regex, can_cache) = match (match_pat.has_captures, captures) {
            (true, Some((region, s))) => {
                with_refs = match_pat.regex_with_refs(region, s);
                (&with_refs, false)
            }
            _ => (match_pat.regex(), true),
        };
        budget.spend_search()?;
        let matched = regex.search(line, start, line.len(), Some(regions));

        if matched {
            let (match_start, match_end) = match_bounds(regions)?;
            let does_something = match match_pat.operation {
                MatchOperation::None => match_start != match_end,
                MatchOperation::Push(_) => self.stack.len() < MAX_STACK_DEPTH,
                _ => true,
            };
            if can_cache && does_something {
                search_cache.insert(match_pat, Some(regions.clone()));
            }
            if does_something {
                return Ok(Some(regions.clone()));
            }
        } else if can_cache {
            search_cache.insert(match_pat, None);
        }
        Ok(None)
    }

    fn exec_pattern<'a>(
        &mut self,
        line: &str,
        reg_match: &RegexMatch<'a>,
        level_context: &'a Context,
        syntax_set: &'a SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<bool, ParsingError> {
        let (match_start, match_end) = match_bounds(&reg_match.regions)?;
        let context = reg_match.context;
        let pat = context.match_at(reg_match.pat_index)?;

        self.push_meta_ops(
            true,
            match_start,
            level_context,
            &pat.operation,
            syntax_set,
            ops,
        )?;
        for s in &pat.scope {
            ops.push((match_start, ScopeStackOp::Push(*s)));
        }
        if let Some(ref capture_map) = pat.captures {
            let mut map: Vec<((usize, i64), ScopeStackOp)> = Vec::new();
            for (cap_index, scopes) in capture_map.iter() {
                if let Some((cap_start, cap_end)) = reg_match.regions.pos(*cap_index) {
                    if cap_start == cap_end {
                        continue;
                    }
                    let length = i64::try_from(cap_end.saturating_sub(cap_start)).unwrap_or(0);
                    for scope in scopes.iter() {
                        map.push(((cap_start, -length), ScopeStackOp::Push(*scope)));
                    }
                    map.push(((cap_end, i64::MIN), ScopeStackOp::Pop(scopes.len())));
                }
            }
            map.sort_by_key(|entry| entry.0);
            for ((index, _), op) in map {
                ops.push((index, op));
            }
        }
        if !pat.scope.is_empty() {
            ops.push((match_end, ScopeStackOp::Pop(pat.scope.len())));
        }
        self.push_meta_ops(
            false,
            match_end,
            level_context,
            &pat.operation,
            syntax_set,
            ops,
        )?;

        self.perform_op(line, &reg_match.regions, pat, syntax_set)
    }

    fn push_meta_ops(
        &self,
        initial: bool,
        index: usize,
        cur_context: &Context,
        match_op: &MatchOperation,
        syntax_set: &SyntaxSet,
        ops: &mut Vec<(usize, ScopeStackOp)>,
    ) -> Result<(), ParsingError> {
        match *match_op {
            MatchOperation::Pop => {
                let v = if initial {
                    &cur_context.meta_content_scope
                } else {
                    &cur_context.meta_scope
                };
                if !v.is_empty() {
                    ops.push((index, ScopeStackOp::Pop(v.len())));
                }
                if !initial && cur_context.clear_scopes.is_some() {
                    ops.push((index, ScopeStackOp::Restore))
                }
            }
            MatchOperation::Push(ref context_refs) | MatchOperation::Set(ref context_refs) => {
                let is_set = matches!(*match_op, MatchOperation::Set(_));
                let contexts = context_refs
                    .iter()
                    .map(|r| r.resolve(syntax_set))
                    .collect::<Result<Vec<&Context>, ParsingError>>()?;
                if initial {
                    if is_set && cur_context.clear_scopes.is_some() {
                        ops.push((index, ScopeStackOp::Restore));
                    }
                    for ctx in &contexts {
                        if !is_set && let Some(clear_amount) = ctx.clear_scopes {
                            ops.push((index, ScopeStackOp::Clear(clear_amount)));
                        }
                        for scope in ctx.meta_scope.iter() {
                            ops.push((index, ScopeStackOp::Push(*scope)));
                        }
                    }
                } else {
                    let repush = (is_set
                        && (!cur_context.meta_scope.is_empty()
                            || !cur_context.meta_content_scope.is_empty()))
                        || contexts.iter().any(|ctx| {
                            !ctx.meta_content_scope.is_empty()
                                || (ctx.clear_scopes.is_some() && is_set)
                        });
                    if repush {
                        let mut num_to_pop: usize =
                            contexts.iter().map(|ctx| ctx.meta_scope.len()).sum();
                        if is_set {
                            num_to_pop +=
                                cur_context.meta_content_scope.len() + cur_context.meta_scope.len();
                        }
                        if num_to_pop > 0 {
                            ops.push((index, ScopeStackOp::Pop(num_to_pop)));
                        }
                        for ctx in &contexts {
                            if is_set && let Some(clear_amount) = ctx.clear_scopes {
                                ops.push((index, ScopeStackOp::Clear(clear_amount)));
                            }
                            for scope in ctx.meta_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                            }
                            for scope in ctx.meta_content_scope.iter() {
                                ops.push((index, ScopeStackOp::Push(*scope)));
                            }
                        }
                    }
                }
            }
            MatchOperation::None => (),
        }
        Ok(())
    }

    fn perform_op(
        &mut self,
        line: &str,
        regions: &Region,
        pat: &MatchPattern,
        syntax_set: &SyntaxSet,
    ) -> Result<bool, ParsingError> {
        let (ctx_refs, old_proto_ids) = match pat.operation {
            MatchOperation::Push(ref ctx_refs) => (ctx_refs, None),
            MatchOperation::Set(ref ctx_refs) => (ctx_refs, self.stack.pop().map(|s| s.prototypes)),
            MatchOperation::Pop => {
                self.stack.pop();
                return Ok(true);
            }
            MatchOperation::None => return Ok(false),
        };
        let last = ctx_refs.len().saturating_sub(1);
        for (i, r) in ctx_refs.iter().enumerate() {
            let mut proto_ids = if i == 0 {
                old_proto_ids.clone().unwrap_or_default()
            } else {
                Vec::new()
            };
            if i == last
                && let Some(ref p) = pat.with_prototype
            {
                proto_ids.push(p.id()?);
            }
            let context_id = r.id()?;
            let context = syntax_set.get_context(&context_id)?;
            let uses_backrefs = context.uses_backrefs
                || proto_ids
                    .iter()
                    .any(|id| syntax_set.get_context(id).is_ok_and(|c| c.uses_backrefs));
            let captures = uses_backrefs.then(|| (regions.clone(), line.to_owned()));
            self.stack.push(StateLevel {
                context: context_id,
                prototypes: proto_ids,
                captures,
            });
        }
        Ok(true)
    }
}
