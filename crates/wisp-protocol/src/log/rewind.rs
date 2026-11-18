use std::collections::BTreeMap;

use crate::{CacheTtl, Message, RewindPoint};

use super::{BlockPosition, LogEvent, LoggedRequest, RebuildError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuiltPoint {
    pub turn: u64,
    pub source: usize,
    pub messages: usize,
    pub blocks: usize,
    pub opening: Option<BlockPosition>,
    pub cuts: usize,
    pub head: Option<usize>,
    pub join: Option<usize>,
    pub epoch: u64,
    pub anchor: Option<LoggedRequest>,
    pub baseline: Option<LoggedRequest>,
    pub refreshed: Vec<(String, CacheTtl)>,
}

struct Rewind {
    seq: u64,
    to: u64,
    turn: u64,
}

fn refused(turn: u64, seq: Option<u64>, reason: String) -> RebuildError {
    RebuildError {
        kind: super::DamageKind::MalformedHistory,
        turn,
        sha256: String::new(),
        reason,
        rewind_at: seq,
    }
}

pub const REWIND_NEEDS_RECORDS: &str =
    "a rewind needs record numbers, so this log can only be rebuilt from its records";

fn rewinds(records: &[(u64, &LogEvent)]) -> Result<Vec<Rewind>, RebuildError> {
    let mut found = Vec::new();
    for (seq, event) in records {
        let LogEvent::Rewound { to, point, .. } = event else {
            continue;
        };
        let turn = point.turn();
        if *to >= *seq {
            return Err(refused(
                turn,
                Some(*seq),
                format!("the rewind at record {seq} keeps record {to}, which isn't before it"),
            ));
        }
        found.push(Rewind {
            seq: *seq,
            to: *to,
            turn,
        });
    }
    Ok(found)
}

pub fn rewind_mask(records: &[(u64, &LogEvent)]) -> Result<Vec<bool>, RebuildError> {
    let found = rewinds(records)?;
    let mut applied: Vec<&Rewind> = Vec::new();
    for rewind in found.iter().rev() {
        if !applied
            .iter()
            .any(|r| r.to < rewind.seq && rewind.seq < r.seq)
        {
            applied.push(rewind);
        }
    }
    for later in &applied {
        if let Some(earlier) = found
            .iter()
            .find(|r| r.seq < later.seq && r.to < later.to && later.to < r.seq)
        {
            return Err(refused(
                later.turn,
                Some(later.seq),
                format!(
                    "the rewind at record {} goes back to record {}, inside the turns the rewind at record {} left",
                    later.seq, later.to, earlier.seq
                ),
            ));
        }
    }
    let mut masked = vec![false; records.len()];
    for rewind in &applied {
        for (slot, (seq, _)) in masked.iter_mut().zip(records) {
            if rewind.to < *seq && *seq < rewind.seq {
                *slot = true;
            }
        }
    }
    Ok(masked)
}

pub const BYTE_REASONS: &[&str] = &[
    "native_search_refused",
    "tools_change",
    "system_change",
    "tail_closed",
    "thinking_cut",
    "rendering_form",
    "search_history_unreadable",
    "search_history_rendered",
    "attachments_as_notes",
];

pub(super) fn changes_bytes(reason: &str) -> bool {
    BYTE_REASONS.contains(&reason)
}

pub(super) fn last_len(history: &[Message]) -> usize {
    history.last().map_or(0, |m| m.content.len())
}

pub(super) fn check_rewind(
    point: RewindPoint,
    seq: Option<u64>,
    history: &[Message],
    (messages, blocks): (usize, usize),
) -> Result<(), RebuildError> {
    if seq.is_none() {
        return Err(refused(point.turn(), None, REWIND_NEEDS_RECORDS.to_owned()));
    }
    let (held, last) = (history.len(), last_len(history));
    if held == messages && last == blocks {
        return Ok(());
    }
    Err(refused(
        point.turn(),
        seq,
        format!(
            "the rewind keeps {messages} messages ending in {blocks} blocks, but the kept history has {held} ending in {last}"
        ),
    ))
}

#[derive(Default)]
pub(super) struct Points {
    pub(super) list: Vec<RebuiltPoint>,
    pub(super) restored: Option<Option<LoggedRequest>>,
    pub(super) masked: BTreeMap<u64, Option<LoggedRequest>>,
}

impl Points {
    pub(super) fn settle(&mut self, history: &[Message]) {
        let unchanged = self
            .list
            .last()
            .is_some_and(|p| p.messages == history.len() && p.blocks == last_len(history));
        if unchanged {
            self.list.pop();
        }
    }

    pub(super) fn start(&mut self, point: RebuiltPoint, history: &[Message]) {
        self.settle(history);
        self.list.push(point);
    }

    pub(super) fn opened(&mut self, turn: u64, at: BlockPosition) {
        if let Some(point) = self
            .list
            .last_mut()
            .filter(|p| p.turn == turn && p.opening.is_none())
        {
            point.opening = Some(at);
        }
    }

    pub(super) fn refresh(&mut self, request: &LoggedRequest, epoch: u64) {
        for point in &mut self.list {
            let same = point.epoch == epoch
                && point.baseline.as_ref().is_some_and(|b| {
                    b.endpoint_name == request.endpoint_name
                        && b.model == request.model
                        && b.effort == request.effort
                });
            if same {
                point.refreshed.retain(|(_, ttl)| *ttl != request.ttl);
                point
                    .refreshed
                    .push((request.started_at.clone(), request.ttl));
            }
        }
    }

    pub(super) fn rewound(&mut self, turn: u64) {
        self.restored = self.masked.get(&turn).cloned();
        self.list.retain(|p| p.turn < turn);
    }
}

pub(super) fn opening_at(history: &[Message], boundary: Option<usize>) -> BlockPosition {
    let len = history.len();
    match history.last() {
        Some(last) if last.role == crate::Role::User && boundary != Some(len) => BlockPosition {
            message: len - 1,
            block: last.content.len(),
        },
        _ => BlockPosition {
            message: len,
            block: 0,
        },
    }
}
