use super::*;
use crate::test_guard::{mail, state};

async fn drained(news: &News) -> Vec<TreeNews> {
    let mut out = Vec::new();
    while news.queue().items.front().is_some() {
        out.extend(news.next().await);
    }
    out
}

#[tokio::test]
async fn a_newer_state_replaces_the_agents_waiting_one_unless_something_about_it_came_after() {
    let news = News::default();
    news.push(state("/a", 1.0));
    news.push(mail("/b", "/c"));
    news.push(state("/a", 2.0));
    news.push(state("/b", 1.0));
    news.push(mail("/a", "/b"));
    news.push(state("/a", 3.0));
    news.push(state("/b", 2.0));
    assert_eq!(
        drained(&news).await,
        [
            mail("/b", "/c"),
            state("/a", 2.0),
            state("/b", 1.0),
            mail("/a", "/b"),
            state("/a", 3.0),
            state("/b", 2.0),
        ]
    );
}

#[tokio::test]
async fn a_closed_queue_drops_what_waits_and_what_comes_after() {
    let news = News::default();
    news.push(state("/a", 1.0));
    news.close();
    news.push(state("/b", 1.0));
    assert_eq!(news.next().await, None);
}
