use super::*;
use wisp_protocol::RenderingForm;

const TAMPERED: &str = "messages.1.content.0: Invalid `signature` in `thinking` block";
const REDACTED: &str = "messages.1.content.0: Invalid `data` in `redacted_thinking` block";

fn misses(bench: &Bench) -> Vec<wisp_protocol::MissCause> {
    log_events(&bench.store)
        .into_iter()
        .filter_map(|e| match e {
            LogEvent::CacheMiss { cause, .. } => cause,
            _ => None,
        })
        .collect()
}

fn expected(reason: &str) -> wisp_protocol::MissCause {
    wisp_protocol::MissCause::Expected {
        reason: reason.into(),
    }
}

fn refusals(bench: &Bench) -> Vec<usize> {
    bench
        .served()
        .iter()
        .enumerate()
        .filter(|(_, s)| s.status == 400)
        .map(|(i, _)| i)
        .collect()
}

fn whole_cuts(bench: &Bench) -> usize {
    bench
        .cuts()
        .iter()
        .filter(|(from, _, _)| *from == WHOLE)
        .count()
}

async fn quit_and_resume(bench: &Bench, live: Live, build: Build) -> Live {
    finish(bench, live).await;
    bench.resume(build)
}

#[tokio::test]
async fn refused_thinking_is_cut_once_resent_and_resumes_with_the_same_bytes() {
    let prefix_refused: &'static str = format!("messages.1.content.0: {BOUND_ELSEWHERE}").leak();
    for (name, wording) in [
        ("prefix", prefix_refused),
        ("tampered", TAMPERED),
        ("redacted", REDACTED),
    ] {
        let mut sent = Vec::new();
        for quit in [false, true] {
            let case = format!("refused-thinking-{name}-{quit}");
            let bench = Bench::new(&case, false).await;
            let build = Build::default();
            let mut live = bench.start(build);
            live.completed("u1", "Look around.").await;
            live.completed("u2", &"word ".repeat(20_000)).await;
            bench.api.lock().unwrap().tamper(wording);
            let change = bench.settled();
            live.completed("u3", "And again.").await;
            if quit {
                live = quit_and_resume(&bench, live, build).await;
            }
            two_turns(&mut live, 4).await;
            finish(&bench, live).await;

            let served = bench.served();
            let refused = refusals(&bench);
            assert_eq!(refused.len(), 1, "{case}");
            let resend = &served[refused[0] + 1];
            assert!(!sends_thinking(&resend.body), "{case}");
            assert!(binds(&resend.body), "{case}");
            assert_eq!(bench.cuts().len(), 1, "{case}: {:?}", bench.cuts());
            assert_eq!(whole_cuts(&bench), 1, "{case}");
            assert_eq!(marks(&bench), ["thinking_cut"], "{case}");
            assert_eq!(misses(&bench), vec![expected("thinking_cut")], "{case}");
            assert_settles(&bench, &[change]);
            let ok = settled(&served);
            sent.push(ok[change + 3].body.clone());
        }
        assert_eq!(sent[0], sent[1], "{name}: the resumed request differs");
    }
}

#[tokio::test]
async fn refused_thinking_after_y1_turned_binding_controls_on_sets_them_back_and_resumes_with_the_same_bytes()
 {
    let mut sent = Vec::new();
    for quit in [false, true] {
        let case = format!("refused-thinking-after-y1-{quit}");
        let bench = Bench::new(&case, false).await;
        {
            let mut api = bench.api.lock().unwrap();
            api.iterations = true;
            api.search_once = true;
        }
        let build = Build {
            search: true,
            before_binding: true,
            ..Build::default()
        };
        let mut live = bench.start(build);
        live.completed("u1", "Look it up.").await;
        live.completed("u2", &"word ".repeat(20_000)).await;
        {
            let mut api = bench.api.lock().unwrap();
            api.unreadable = true;
            api.tamper(TAMPERED);
        }
        let change = bench.settled();
        live.completed("u3", "And again.").await;
        let later = Build {
            before_binding: false,
            ..build
        };
        if quit {
            live = quit_and_resume(&bench, live, later).await;
        }
        two_turns(&mut live, 4).await;
        let agent = finish(&bench, live).await;
        assert!(!agent.settings.binding_controls, "{case}");
        assert_eq!(agent.search.kept_break, None, "{case}");

        let served = bench.served();
        let refused = refusals(&bench);
        assert_eq!(refused.len(), 4, "{case}: the Y1 refusal, then the ladder");
        assert!(
            binds(&served[refused[1]].body),
            "{case}: Y1 turned binding on"
        );
        let resend = &served[refused[3] + 1];
        assert!(!sends_thinking(&resend.body), "{case}");
        assert!(!binds(&resend.body), "{case}");
        assert_eq!(bench.cuts().len(), 1, "{case}: {:?}", bench.cuts());
        assert_eq!(whole_cuts(&bench), 1, "{case}");
        let ok = settled(&served);
        assert!(ok[change..].iter().all(|s| !binds(&s.body)), "{case}");
        assert_settles(&bench, &[change]);
        sent.push(ok[change + 3].body.clone());
    }
    assert_eq!(sent[0], sent[1], "the resumed request differs");
}

#[tokio::test]
async fn a_rewind_past_a_conversion_without_binding_controls_recovers_through_the_ladder() {
    let bench = Bench::new("refused-thinking-rewound", false).await;
    {
        let mut api = bench.api.lock().unwrap();
        api.iterations = true;
        api.search_once = true;
    }
    let build = Build {
        search: true,
        model: "claude-opus-4-5",
        ..Build::default()
    };
    let mut live = bench.start(build);
    live.completed("u1", "Look it up.").await;
    live.completed("u2", "And again.").await;
    bench.api.lock().unwrap().unreadable = true;
    live.completed("u3", "Once more.").await;
    assert_eq!(bench.cuts().len(), 1, "{:?}", bench.cuts());
    let before = refusals(&bench).len();
    live.send(ClientCommand::Rewind {
        to: wisp_protocol::RewindPoint::Turn { turn: 3 },
        request: RequestId(2),
    })
    .await;
    let done = live
        .until(|e| {
            matches!(e, AgentEvent::Rewound { .. }) || matches!(e, AgentEvent::Rejected { .. })
        })
        .await;
    assert!(matches!(done, AgentEvent::Rewound { .. }), "{done:?}");
    live.completed("u4", "Once more, again.").await;
    finish(&bench, live).await;
    assert_eq!(refusals(&bench).len(), before + 1);
    let forms: Vec<RenderingForm> = pick!(
        log_events(&bench.store),
        LogEvent::RenderingForm { form } => form
    );
    assert_eq!(forms, [RenderingForm::WithoutThinking]);
    assert_eq!(whole_cuts(&bench), 0);
}

#[tokio::test]
async fn a_compaction_refused_for_thinking_right_after_a_resume_keeps_the_logged_binding_controls()
{
    let mut sent = Vec::new();
    for quit in [false, true] {
        let case = format!("refused-thinking-compacted-after-resume-{quit}");
        let bench = Bench::new(&case, false).await;
        let build = Build::default();
        let mut live = bench.start(build);
        two_turns(&mut live, 1).await;
        finish(&bench, live).await;
        let mut live = bench.resume(build);
        bench.api.lock().unwrap().tamper(TAMPERED);
        live.compacted(1).await;
        if quit {
            live = quit_and_resume(&bench, live, build).await;
        }
        let before = bench.served().len();
        live.completed("u3", "After the compaction.").await;
        finish(&bench, live).await;
        assert_eq!(whole_cuts(&bench), 1, "{case}");
        let first = &bench.served()[before];
        assert!(binds(&first.body), "{case}");
        sent.push(first.body.clone());
    }
    assert_eq!(sent[0], sent[1], "the resumed request differs");
}
