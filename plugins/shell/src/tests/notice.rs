use wisp_plugin_api::hooks::{Event, Reply};
use wisp_plugin_api::host::JobEnd;
use wisp_plugin_api::prompt::{EndedJob, Item, News};
use wisp_plugin_api::types::ExitStatus;
use wisp_plugin_testkit::hooks::before_turn;
use wisp_tool_text::jobs::{WAKE_LINE, item_line};

use super::*;

fn ended(id: u32) -> Item {
    Item::Ended(EndedJob {
        id,
        target: format!("job {id}"),
        end: JobEnd::Exited(ExitStatus::Code(0)),
        unread: 0,
    })
}

fn hooked(news: Option<News>, hook: &str) -> Reply {
    let shell = Shell::new(Arc::new(FakeHost {
        news,
        ..FakeHost::default()
    }));
    block_on(shell.on_hook(hook, Event::BeforeTurn(before_turn(1, ""))))
}

#[test]
fn the_notice_holds_each_line_in_order_inside_its_tags() {
    let news = News {
        wake: false,
        items: vec![Item::Typed(2), ended(3)],
    };
    let lines = [item_line(&news.items[0]), item_line(&news.items[1])].map(Option::unwrap);
    assert_eq!(
        notice(&news),
        Some(format!(
            "<processes>\n{}\n{}\n</processes>",
            lines[0], lines[1]
        ))
    );
}

#[test]
fn a_wake_turn_leads_with_the_wake_line_and_only_with_other_news() {
    let news = News {
        wake: true,
        items: vec![ended(1)],
    };
    let text = notice(&news).unwrap();
    assert!(
        text.starts_with(&format!("<processes>\n{WAKE_LINE}\nProcess 1 (job 1)")),
        "{text}"
    );
    let empty = News {
        wake: true,
        items: vec![Item::Restarted(Vec::new())],
    };
    assert_eq!(notice(&empty), None);
}

#[test]
fn the_processes_hook_appends_the_notice_from_the_opening_and_nothing_without_news() {
    let news = News {
        wake: false,
        items: vec![ended(4)],
    };
    assert_eq!(
        hooked(Some(news.clone()), PROCESSES),
        Reply::Append(vec![notice(&news).unwrap()])
    );
    assert_eq!(hooked(None, PROCESSES), Reply::None);
    assert_eq!(hooked(Some(news), "other"), Reply::None);
}

#[test]
fn a_notice_for_300_ended_jobs_is_whole() {
    let news = News {
        wake: true,
        items: (1..=300).map(ended).collect(),
    };
    let text = notice(&news).unwrap();
    assert!(text.chars().count() > 10_000, "{}", text.len());
    assert_eq!(text.lines().count(), 303);
    assert!(text.contains("Process 300 (job 300) exited with code 0."));
}

#[test]
fn the_manifest_declares_the_processes_hook_in_the_news_set() {
    let manifest = wisp_plugin_api::manifest::Manifest::parse(
        MANIFEST,
        wisp_plugin_api::manifest::Origin::Bundled,
    )
    .unwrap();
    assert_eq!(manifest.world, "0.2.0");
    let hook = manifest.hooks.iter().find(|h| h.name == PROCESSES).unwrap();
    assert_eq!(hook.point, "before-turn");
    assert_eq!(hook.sets, ["core/news"]);
    assert_eq!(hook.retry, wisp_plugin_api::manifest::Retry::Replay);
    assert!(hook.job_news);
    assert_eq!(hook.continues.as_deref(), Some("core/processes"));
    assert_eq!(hook.switch, None);
}
