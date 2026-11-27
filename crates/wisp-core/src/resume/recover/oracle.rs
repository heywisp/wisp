use std::collections::BTreeSet;

use wisp_protocol::log::{Damage, DamageKind, LogEvent, LogRecord};

use super::{
    FellBack, Plan, Providers, Ranges, Recovered, carried, dropped_processes, dropped_records,
    head_start, later_damage, lost_split, masks, returned_in, seen, seq_of, spent, started,
    turn_of,
};
use crate::session::{Line, SessionStore};

fn effective(lines: &[Line], ranges: &Ranges) -> Vec<LogRecord> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Record(record) if !ranges.dropped(seq_of(index)) || carried(&record.event) => {
                Some(record.as_ref().clone())
            }
            _ => None,
        })
        .collect()
}

fn readable(lines: &[Line], ranges: &Ranges) -> Vec<LogRecord> {
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Misplaced(record) if !ranges.dropped(seq_of(index)) => {
                Some(record.as_ref().clone())
            }
            Line::Record(record) if !ranges.dropped(seq_of(index)) || carried(&record.event) => {
                Some(record.as_ref().clone())
            }
            _ => None,
        })
        .collect()
}

fn read_damage(lines: &[Line], ranges: &Ranges, records: &[LogRecord]) -> Vec<Damage> {
    let mut damage: Vec<Damage> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match line {
            Line::Damaged(kind) if !ranges.dropped(seq_of(index)) => Some(Damage {
                kind: *kind,
                seq: seq_of(index),
                turn: None,
                sha256: None,
            }),
            Line::Misplaced(record) if !ranges.dropped(seq_of(index)) => Some(Damage {
                kind: DamageKind::OutOfOrder,
                seq: seq_of(index),
                turn: turn_of(&record.event),
                sha256: None,
            }),
            _ => None,
        })
        .collect();
    let first = records.iter().find(|r| !carried(&r.event));
    match first {
        Some(record) if matches!(record.event, LogEvent::SessionStarted { .. }) => {}
        Some(record) => damage.push(Damage {
            kind: DamageKind::NoStart,
            seq: record.seq,
            turn: turn_of(&record.event),
            sha256: None,
        }),
        None => {}
    }
    damage.sort_by_key(|d| d.seq);
    damage
}

fn cut(records: &[LogRecord], point: u64) -> Vec<LogRecord> {
    let mut point = point;
    loop {
        let kept = cut_at(records, point);
        let last = kept.last().map(|r| r.seq);
        let back = records
            .iter()
            .filter(|r| last.is_none_or(|l| r.seq > l))
            .filter_map(|r| match r.event {
                LogEvent::Rewound { to, .. } if last.is_some_and(|l| to < l) => {
                    Some(to.saturating_add(1))
                }
                _ => None,
            })
            .min();
        match back {
            Some(earlier) if earlier < point => point = earlier,
            _ => return kept,
        }
    }
}

fn cut_at(records: &[LogRecord], point: u64) -> Vec<LogRecord> {
    let prefix: Vec<&LogRecord> = records.iter().filter(|r| r.seq < point).collect();
    let clean = prefix
        .iter()
        .filter_map(|r| match r.event {
            LogEvent::TurnEnded { turn, .. } => Some(turn),
            _ => None,
        })
        .max();
    let end = prefix
        .iter()
        .position(|r| turn_of(&r.event).is_some_and(|t| clean.is_none_or(|c| t > c)))
        .unwrap_or(prefix.len());
    prefix[..end].iter().map(|r| (*r).clone()).collect()
}

fn plan(all: &[LogRecord], damage: &[Damage], limit: Option<u64>) -> Plan<LogRecord> {
    for k in (0..=damage.len()).rev() {
        let point = match (damage.get(k).map(|d| d.seq), limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let records = match point {
            Some(point) => cut(all, point),
            None => all.to_vec(),
        };
        let covered = match k.checked_sub(1) {
            None => {
                return Plan {
                    records,
                    start: None,
                    covered: None,
                    point,
                };
            }
            Some(last) => &damage[..=last],
        };
        if covered.iter().any(|d| d.kind == DamageKind::NoStart) || !started(&records) {
            continue;
        }
        let Some(through) = covered.last().map(|d| d.seq) else {
            continue;
        };
        if let Some(start) = head_start(&records, through) {
            return Plan {
                records,
                start: Some(start),
                covered: Some(through),
                point,
            };
        }
    }
    Plan {
        records: Vec::new(),
        start: None,
        covered: None,
        point: Some(0),
    }
}

pub(in crate::resume) fn recover(
    lines: &[Line],
    store: &dyn SessionStore,
    providers: Providers<'_>,
) -> Result<Recovered, String> {
    if lines.is_empty() {
        return Err("the log is empty".to_owned());
    }
    let next_seq = seq_of(lines.len());
    let ranges = Ranges::read(lines);
    let all = effective(lines, &ranges);
    let readable = readable(lines, &ranges);
    let readable_masks = masks(&readable);
    let damage = read_damage(lines, &ranges, &all);
    let mut rounds: Vec<Damage> = Vec::new();
    let mut limit: Option<u64> = None;
    let (plan, lost, checked) = loop {
        let plan = plan(&all, &damage, limit);
        let borrowed: Vec<&LogRecord> = plan.records.iter().collect();
        match later_damage(&borrowed, plan.start, store, providers)? {
            (None, lost, checked) => break (plan, lost, checked),
            (Some(found), ..) => {
                limit = Some(limit.map_or(found.seq, |l| l.min(found.seq)));
                rounds.push(found);
            }
        }
    };
    let new_cover = plan
        .covered
        .filter(|through| ranges.covered.is_none_or(|old| through > &old));
    let covered = new_cover.and_then(|_| {
        damage
            .iter()
            .find(|d| ranges.covered.is_none_or(|old| d.seq > old))
            .cloned()
    });
    let at_point = damage
        .iter()
        .find(|d| plan.point == Some(d.seq) && Some(*d) != covered.as_ref())
        .cloned();
    let mut found: Vec<Damage> = covered.into_iter().chain(at_point).collect();
    found.extend(rounds);
    let restart = plan.point.is_none() && new_cover.is_none() && !started(&plan.records);
    let point = if restart { Some(next_seq) } else { plan.point };
    if point.is_none() && new_cover.is_none() {
        return Ok(Recovered {
            start: plan.start,
            records: plan.records,
            lost,
            fell_back: None,
            dropped_processes: dropped_processes(lines, |seq| ranges.dropped(seq)),
            dropped_inputs: returned_in(&dropped_records(lines, |seq| ranges.dropped(seq)), lines),
            checked,
        });
    }
    let fresh = point.is_some() && !started(&plan.records);
    let cut_records = point.is_some();
    let kept_through = if fresh {
        None
    } else {
        plan.records.last().map(|r| r.seq)
    };
    let last_turn = plan
        .records
        .iter()
        .filter_map(|r| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .max();
    let (turns_seen, processes_seen) = seen(lines);
    let mut records = if fresh { Vec::new() } else { plan.records };
    let after = |r: &&LogRecord| kept_through.is_none_or(|k| r.seq > k);
    let (dropped, settings_dropped, returned) = if cut_records {
        records.extend(
            all.iter()
                .filter(after)
                .filter(|r| carried(&r.event))
                .cloned(),
        );
        let dropped: Vec<LogRecord> = readable.iter().filter(after).cloned().collect();
        let live: Vec<LogRecord> = readable
            .iter()
            .zip(&readable_masks)
            .filter(|(r, masked)| !**masked && after(r))
            .map(|(r, _)| r.clone())
            .collect();
        let settings = dropped
            .iter()
            .any(|r| matches!(r.event, LogEvent::SettingsChanged { .. }));
        let returned = returned_in(&live.iter().collect::<Vec<_>>(), lines);
        (true, settings, returned)
    } else {
        (false, false, Vec::new())
    };
    let spent_usd = if dropped {
        (spent(&readable) - spent(&records)).max(0.0)
    } else {
        0.0
    };
    let (plan_usd, billing) = if dropped {
        lost_split(&readable, &records, |r| after(&r))
    } else {
        (None, wisp_protocol::Billing::Metered)
    };
    let next_turn = last_turn.map_or(1, |t| t + 1);
    let rewound: BTreeSet<u64> = readable
        .iter()
        .zip(&readable_masks)
        .filter(|(_, masked)| **masked)
        .filter_map(|(r, _)| match r.event {
            LogEvent::TurnStarted { turn, .. } => Some(turn),
            _ => None,
        })
        .collect();
    let dropped_turns: Vec<u64> = if cut_records {
        (next_turn..=turns_seen)
            .filter(|t| !rewound.contains(t))
            .collect()
    } else {
        Vec::new()
    };
    let event = LogEvent::ResumeFellBack {
        kept_through,
        dropped_through: cut_records.then(|| next_seq.saturating_sub(1)),
        covered_through: new_cover,
        last_turn,
        turns_seen,
        processes_seen,
        spent_usd,
        plan_usd,
        billing,
        damage: found,
    };
    let start = if fresh { None } else { plan.start };
    let lost = if fresh { Vec::new() } else { lost };
    let dropped_processes = dropped_processes(lines, |seq| {
        ranges.dropped(seq) || (cut_records && kept_through.is_none_or(|k| seq > k))
    });
    Ok(Recovered {
        records,
        start,
        lost,
        dropped_processes,
        dropped_inputs: returned_in(&dropped_records(lines, |seq| ranges.dropped(seq)), lines),
        fell_back: Some(FellBack {
            event,
            seq: next_seq,
            fresh,
            last_turn,
            dropped_turns,
            cut: cut_records,
            covered: new_cover.is_some(),
            settings_dropped,
            returned,
        }),
        checked: None,
    })
}
