#![allow(
    dead_code,
    reason = "render's and tui's library tests and the budgets and memory binaries each use part of the code pump"
)]

use std::collections::BTreeMap;
use std::sync::Arc;

use wisp_render::highlight::{CodeUpdate, CodeWorker, Grammars, Highlighter};
use wisp_render::stream::CodeFeed;
use wisp_render::transcript::Transcript;

pub(crate) struct Code {
    grammars: Arc<Grammars>,
    workers: BTreeMap<usize, CodeWorker>,
}

impl Code {
    pub(crate) fn new(grammars: Arc<Grammars>) -> Code {
        Code {
            grammars,
            workers: BTreeMap::new(),
        }
    }

    pub(crate) fn feed(&mut self, stream: usize, feed: &CodeFeed) -> Vec<CodeUpdate> {
        if feed.restart {
            self.workers.remove(&stream);
        }
        let worker = self
            .workers
            .entry(stream)
            .or_insert_with(|| CodeWorker::new(Highlighter::ready(Arc::clone(&self.grammars))));
        worker.retire(feed.retire_below);
        worker.update(&feed.deltas)
    }

    pub(crate) fn keep_live(&mut self, transcript: &Transcript) {
        let live = transcript.code_streams();
        self.workers.retain(|stream, _| live.contains(stream));
    }

    pub(crate) fn pump(&mut self, transcript: &mut Transcript) -> usize {
        let updates: Vec<(usize, Vec<CodeUpdate>)> = transcript
            .take_code()
            .into_iter()
            .map(|(stream, feed)| (stream, self.feed(stream, &feed)))
            .collect();
        self.keep_live(transcript);
        let mut applied = 0;
        for (stream, updates) in updates {
            for update in updates {
                applied += usize::from(transcript.apply_code(stream, update));
            }
        }
        applied
    }
}
