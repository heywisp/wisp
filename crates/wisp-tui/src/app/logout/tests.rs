use super::*;
use crate::testing::texts;
use wisp_render::chrome::account::Asked;

fn key(code: KeyCode) -> Key {
    Key::plain(code)
}

#[test]
fn a_card_drops_keys_until_it_is_drawn_and_answers_by_enter_or_digit() {
    let names = vec!["work-api".to_owned()];
    let named = vec![("work-api".to_owned(), Some(5))];
    let asked = Asked {
        names: named.clone(),
        keys: named,
        ..Asked::default()
    };
    let mut card = SignOut::yes(asked.clone(), Vec::new(), None);
    assert_eq!(card.key(key(KeyCode::Enter)), None);
    card.drawn = true;
    assert_eq!(card.key(key(KeyCode::Enter)), Some(Intent::Close));
    assert_eq!(card.click(CardHit::Option(0)), None);
    assert_eq!(card.key(key(KeyCode::Char('y'))), None);
    let sent = Some(Intent::Send {
        names,
        confirmed: true,
        chat: None,
        follow: Vec::new(),
        gone: false,
    });
    assert_eq!(card.key(key(KeyCode::Enter)), sent);
    let mut card = SignOut::yes(asked, Vec::new(), None);
    card.drawn = true;
    assert_eq!(card.key(key(KeyCode::Char('1'))), None);
    assert_eq!(card.key(key(KeyCode::Char('2'))), Some(Intent::Close));
    let mut fresh = SignOut::yes(Asked::default(), Vec::new(), None);
    assert_eq!(fresh.key(key(KeyCode::Esc)), Some(Intent::Close));
    let row = WayRow {
        kind: "OpenAI API".to_owned(),
        model: "gpt-6.1-sol high".to_owned(),
        price: "cache rebuild ~$0.41".to_owned(),
        consequence: vec!["The charge.".to_owned(), "The reasoning.".to_owned()],
        charges: true,
        ..WayRow::default()
    };
    let heading = (String::new(), String::new(), None);
    let card = SignOut::gone(
        vec!["claude".to_owned()],
        heading.clone(),
        vec![row],
        copy::esc(copy::CANCEL_WORD),
    );
    let glyphs = wisp_render::theme::glyphs::GlyphSet::Unicode;
    let options = card.way_options(&[], Layout::new(80, glyphs), &Glyphs::of(glyphs));
    assert_eq!(
        options[0].description.lines().collect::<Vec<_>>(),
        [
            "OpenAI API · gpt-6.1-sol high",
            "cache rebuild ~$0.41",
            "The charge.",
            "The reasoning.",
        ]
    );
    let way = |chat: &str, charges: bool| WayRow {
        chat: chat.to_owned(),
        charges,
        ..WayRow::default()
    };
    let ways = vec![way("claude-api", true), way("chatgpt", false)];
    let mut card = SignOut::gone(
        Vec::new(),
        heading.clone(),
        ways.clone(),
        copy::esc(copy::QUIT_WORD),
    );
    card.drawn = true;
    assert_eq!(card.pick.focus, Some(1));
    let sent = |chat: &str| {
        Some(Intent::Send {
            names: Vec::new(),
            confirmed: true,
            chat: Some(chat.to_owned()),
            follow: Vec::new(),
            gone: true,
        })
    };
    assert_eq!(card.click(CardHit::Option(0)), None);
    assert_eq!(card.key(key(KeyCode::Enter)), sent("claude-api"));
    assert_eq!(card.click(CardHit::Option(1)), sent("chatgpt"));
    let all = SignOut::gone(
        Vec::new(),
        heading.clone(),
        vec![way("claude-api", true)],
        copy::esc(copy::QUIT_WORD),
    );
    assert_eq!(all.pick.focus, None);

    let layout = Layout::new(80, GlyphSet::Unicode);
    let glyphs = Glyphs::of(GlyphSet::Unicode);
    let lined = |chat: &str, lines: &[&str], price: &str| WayRow {
        chat: chat.to_owned(),
        consequence: lines.iter().map(|line| (*line).to_owned()).collect(),
        price: price.to_owned(),
        ..WayRow::default()
    };
    let drawn = |ways: Vec<WayRow>| {
        let card = SignOut::gone(
            Vec::new(),
            heading.clone(),
            ways,
            copy::esc(copy::BACK_WORD),
        );
        let shared = card.shared();
        let rows = card.way_options(&shared, layout, &glyphs);
        (
            shared,
            rows.into_iter().map(|o| o.description).collect::<Vec<_>>(),
        )
    };
    let (shared, rows) = drawn(vec![
        lined("claude-api", &["Charges", "No carry"], ""),
        lined("claude-work", &["No carry"], ""),
        lined("chatgpt", &["No carry"], ""),
    ]);
    assert_eq!(shared, ["No carry"]);
    assert_eq!(rows[0].lines().skip(1).collect::<Vec<_>>(), ["Charges"]);
    assert!(rows.iter().all(|row| !row.contains("No carry")), "{rows:?}");
    let (shared, rows) = drawn(vec![lined("claude-work", &["No carry"], "")]);
    assert!(shared.is_empty());
    assert!(rows[0].ends_with("No carry"), "{rows:?}");
    let sign_in = WayRow {
        again: true,
        ..lined("claude-subscription", &[], "")
    };
    let (shared, rows) = drawn(vec![
        sign_in,
        lined("claude-work", &["No carry"], ""),
        lined("claude-api", &["No carry"], ""),
    ]);
    assert!(shared.is_empty());
    assert!(
        rows[1..].iter().all(|row| row.ends_with("No carry")),
        "{rows:?}"
    );

    let rebuild = Rebuild::Usd(0.08);
    let price = way_price(Some(rebuild), GlyphSet::Unicode, &[], &[]);
    let card = SignOut::gone(
        Vec::new(),
        heading.clone(),
        vec![WayRow {
            rebuild: Some(rebuild),
            ..lined("claude-work", &[], &price)
        }],
        copy::esc(copy::BACK_WORD),
    );
    let continuing = account::Moved {
        how: account::Move::Continuing,
        name: "claude-work".to_owned(),
        state: AccountState::Subscription,
        from: None,
        model: "opus-5-5".to_owned(),
        effort: None,
        rebuild: card.rebuild_of("claude-work"),
        mark: None,
    };
    let line = account::moved_lines(&continuing, &glyphs, 80).join(" ");
    assert!(line.ends_with(&price), "{line} / {price}");
    assert_eq!(way_price(None, GlyphSet::Unicode, &[], &[]), "");
    let (_, rows) = drawn(vec![lined("claude-work", &[], "")]);
    assert_eq!(rows[0].lines().count(), 1, "{rows:?}");

    let chat = Some(Rebuild::Usd(0.41));
    let follow = vec!["coder".to_owned(), "scout".to_owned()];
    let priced = |scout: Option<Rebuild>| {
        let movers = vec![
            ("coder".to_owned(), Some(Rebuild::Usd(0.08))),
            ("scout".to_owned(), scout),
        ];
        way_price(chat, GlyphSet::Unicode, &follow, &movers)
    };
    assert_eq!(
        priced(Some(Rebuild::Usd(0.02))),
        "cache rebuild ~$0.41, ~$0.08 and ~$0.02"
    );
    assert_eq!(
        priced(None),
        "cache rebuild ~$0.41 for this chat and ~$0.08 for coder; scout has nothing cached yet."
    );
    assert_eq!(priced(Some(Rebuild::Empty)), priced(None));
    let expired = |movers: &[(String, Option<Rebuild>)]| {
        way_price(Some(Rebuild::Expired), GlyphSet::Unicode, &follow, movers)
    };
    let coder = ("coder".to_owned(), Some(Rebuild::Usd(0.08)));
    let scout = |r| ("scout".to_owned(), r);
    assert_eq!(
        expired(&[coder.clone(), scout(Some(Rebuild::Usd(0.02)))]),
        "cache rebuild ~$0.08 for coder and ~$0.02 for scout; cache expired for this chat."
    );
    assert_eq!(
        expired(&[coder, scout(None)]),
        "cache rebuild ~$0.08 for coder; cache expired for this chat; scout has nothing cached yet."
    );
    assert_eq!(
        expired(&[scout(None), ("coder".to_owned(), None)]),
        "cache expired for this chat"
    );
    assert_eq!(
        way_price(chat, GlyphSet::Unicode, &follow, &[]),
        "cache rebuild ~$0.41 for this chat"
    );

    let facts = crate::app::fixture::four_accounts();
    let mut tracker = Tracker::new(&facts);
    tracker.sized(100_000);
    let agent = |name: &str, used: Option<u64>| {
        let mut entry: AgentEntry = serde_json::from_value(serde_json::json!({
            "path": format!("/{name}"),
            "name": name,
            "model": "claude-sonnet-5-5",
            "effort": "low",
            "depth": 1,
            "state": {"state": "idle"},
            "activity": {"type": "idle"},
            "spent_usd": 0.0,
        }))
        .unwrap();
        entry.endpoint = Some("claude-subscription".to_owned());
        entry.context = used.map(|used| wisp_protocol::ContextFill {
            used,
            window: None,
            limit: None,
            estimated: false,
            auto_compact: false,
        });
        entry
    };
    let agents = [agent("coder", Some(30_000)), agent("scout", None)];
    let mut own = facts.clone();
    own.model = "claude-sonnet-5-5".to_owned();
    own.effort = "low".to_owned();
    let mut coder = Tracker::new(&own);
    coder.sized(30_000);
    let movers = agents.each_ref().map(|entry| Mover {
        entry,
        facts: own.clone(),
        tracker: &coder,
    });
    let ways = [Way {
        chat: Some("claude-api".to_owned()),
        follow: follow.clone(),
        ..Way::default()
    }];
    let leaving = Leaving::Chat(Billing::AnthropicPlan);
    let rows = |agents| {
        way_rows(
            &facts,
            &tracker,
            GlyphSet::Unicode,
            &ways,
            leaving,
            agents,
            Duration::ZERO,
        )
    };
    let [row] = rows(Some(&movers)).try_into().unwrap();
    let solo = Way {
        follow: Vec::new(),
        ..ways[0].clone()
    };
    let [alone] = way_rows(
        &own,
        &coder,
        GlyphSet::Unicode,
        &[solo],
        leaving,
        None,
        Duration::ZERO,
    )
    .try_into()
    .unwrap();
    assert!(
        alone.rebuild.is_some_and(|r| matches!(r, Rebuild::Usd(_))),
        "{alone:?}"
    );
    assert_eq!(
        row.movers,
        [
            ("coder".to_owned(), alone.rebuild),
            ("scout".to_owned(), None)
        ]
    );
    let coder_price = alone.rebuild.and_then(|r| amount(&r)).unwrap();
    assert!(
        row.price
            .contains(&format!("{coder_price} for coder; scout has nothing")),
        "{}",
        row.price
    );
    let [resumed] = rows(None).try_into().unwrap();
    assert!(
        resumed.price.ends_with("for this chat"),
        "{}",
        resumed.price
    );
    let card = SignOut::place(
        vec![("claude-subscription".to_owned(), None)],
        vec![row],
        true,
    )
    .presets(vec![PresetGone {
        preset: "coder".to_owned(),
        account: "claude-subscription".to_owned(),
        runs: false,
    }]);
    let rows = card.render(24, layout);
    let text = texts(&rows.lines);
    let preset = text
        .iter()
        .position(|l| l.contains("Preset coder used claude-subscription"));
    let where_ = text.iter().position(|l| l.contains(WHERE_SHOULD_IT_GO));
    assert!(preset > where_ && where_.is_some(), "{text:#?}");
    let words = text.join(" ");
    let words: Vec<&str> = words.split_whitespace().collect();
    let words = words.join(" ");
    let line =
        "Preset coder used claude-subscription; new agents from it won't start on the API. 1";
    assert!(words.contains(line), "{words}");
}
