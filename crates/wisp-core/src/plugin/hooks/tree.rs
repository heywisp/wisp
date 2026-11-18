use std::sync::Arc;

use tokio::task::JoinHandle;

use super::Engine;
use super::engine::Payload;
use crate::agents::news::News;
use crate::plugin::convert::tree_event;

pub struct TreeLane {
    news: Arc<News>,
    task: JoinHandle<()>,
}

impl TreeLane {
    pub fn start(engine: Arc<Engine>, news: Arc<News>) -> Self {
        let queue = news.clone();
        let task = tokio::spawn(async move {
            while let Some(item) = queue.next().await {
                let point = item.point();
                engine.mark(point);
                if engine.attached(point) {
                    engine
                        .clone()
                        .fire_detached(point, Payload::Tree(tree_event(item)))
                        .await;
                    engine.mark(point);
                }
            }
        });
        Self { news, task }
    }
}

impl Drop for TreeLane {
    fn drop(&mut self) {
        self.news.close();
        self.task.abort();
    }
}

#[cfg(test)]
mod tests;
