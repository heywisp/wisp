use serde_json::{Value, json};

use super::*;

pub(crate) const KEY: &str = "0123456789abcdef0123456789abcdef";

pub(crate) fn wire(entries: &[Value]) -> Vec<u8> {
    json!({ "models": entries }).to_string().into_bytes()
}

pub(crate) fn entry(slug: &str, priority: i64, window: u64) -> Value {
    let levels = ["low", "medium", "high", "xhigh", "max", "ultra"]
        .map(|effort| json!({ "effort": effort, "description": "PLACEHOLDER" }));
    json!({
        "slug": slug,
        "visibility": "list",
        "priority": priority,
        "context_window": window,
        "max_context_window": 872_000,
        "supported_reasoning_levels": levels,
        "default_reasoning_level": "low",
        "input_modalities": ["text", "image"],
        "supports_search_tool": true,
        "description": "PLACEHOLDER",
        "base_instructions": "PLACEHOLDER",
        "model_messages": { "instructions_template": "PLACEHOLDER" },
    })
}

pub(crate) fn claude_wire(entries: &[Value]) -> Vec<u8> {
    let page = json!({ "data": entries, "has_more": false, "first_id": null, "last_id": null });
    page.to_string().into_bytes()
}

pub(crate) fn claude_entry(id: &str, line: &str, window: u64) -> Value {
    let on = json!({ "supported": true });
    let effort =
        json!({ "supported": true, "low": on, "medium": on, "high": on, "xhigh": on, "max": on });
    json!({
        "type": "model",
        "id": id,
        "display_name": "PLACEHOLDER",
        "created_at": "PLACEHOLDER",
        "lifecycle": "active",
        "line": line,
        "max_input_tokens": window,
        "max_tokens": 128_000,
        "capabilities": {
            "effort": effort,
            "thinking": { "supported": true, "types": { "adaptive": on, "enabled": on } },
            "image_input": on,
            "pdf_input": on,
            "server_tools": { "supported": true, "web_search": on },
        },
    })
}

pub(crate) fn claude_sample() -> ModelList {
    let entries = [
        claude_entry("claude-opus-6", "opus", 1_000_000),
        claude_entry("claude-opus-5-5", "opus", 500_000),
        claude_entry("claude-haiku-5-5", "haiku", 1_000_000),
    ];
    anthropic::parse(&claude_wire(&entries)).unwrap()
}

pub(crate) fn sample() -> ModelList {
    ModelList::parse(&wire(&[
        entry("gpt-6.1-sol", 1, 500_000),
        entry("gpt-new", 2, 272_000),
    ]))
    .unwrap()
}

#[test]
fn gemini_lists_keep_only_limits_and_ignore_saved_copies_of_shipped_facts() {
    use crate::context::{ListedRow, ModelSettings};
    use wisp_protocol::context::WindowSource;

    let kind = ProviderKind::Gemini;
    let id = "gemini-4-carbon";
    let shipped = ModelTable::shared().find(id).unwrap();
    let caps = shipped.capabilities.unwrap();
    for (input, output) in [
        (500_000, 65_535),
        (500_000, COMPACTION_MAX_TOKENS),
        (1, COMPACTION_MAX_TOKENS - 1),
    ] {
        let list =
            ModelList::from_gemini_ids([(id.to_owned(), Some(input), Some(output))], None).unwrap();
        let entry = list.find(id).unwrap();
        assert!(entry.levels.is_empty());
        assert_eq!(entry.default_level, None);
        assert_eq!(entry.adaptive, None);
        assert!(!entry.images && !entry.search);
        let expected = ModelCapabilities {
            max_output: if output >= COMPACTION_MAX_TOKENS {
                Some(output)
            } else {
                caps.max_output
            },
            ..caps
        };
        assert_eq!(
            list.capabilities(kind, id, Some(caps), crate::provider::gemini::LISTED_ONLY),
            Some(expected)
        );
        let row = entry.row(kind);
        assert_eq!(row.web_search, None);
        assert_eq!(row.context_window, (input != 1).then_some(input));
        let listed = ListedRow {
            row: row.clone(),
            source: WindowSource::Subscription,
            max_window: None,
        };
        let window = ModelSettings::shipped_only()
            .resolve("gemini", id, Some(&listed), 64_000)
            .window
            .unwrap();
        assert_eq!(
            Some(window.tokens),
            if input == 1 {
                shipped.context_window
            } else {
                Some(input)
            }
        );
        let mut saved = serde_json::to_value(&list).unwrap();
        saved[0]["levels"] = json!(["max"]);
        saved[0]["default_level"] = json!("max");
        saved[0]["adaptive"] = json!(false);
        saved[0]["images"] = json!(false);
        saved[0]["search"] = json!(true);
        let old: ModelList = serde_json::from_value(saved).unwrap();
        assert_eq!(
            old.capabilities(kind, id, Some(caps), crate::provider::gemini::LISTED_ONLY),
            Some(expected)
        );
        assert_eq!(old.find(id).unwrap().row(kind), row);
    }
    let id = "gemini-unshipped-small";
    let small = 8_192;
    for (output, expected) in [(small, Some(small)), (0, None)] {
        let list = ModelList::from_gemini_ids([(id.to_owned(), Some(500_000), Some(output))], None)
            .unwrap();
        assert_eq!(list.find(id).unwrap().max_output, expected);
        let caps = list
            .capabilities(kind, id, None, crate::provider::gemini::LISTED_ONLY)
            .unwrap();
        assert_eq!(caps.max_output, expected);
    }
}

#[test]
fn a_list_is_read_in_priority_order_keeping_only_the_fields_wisp_reads() {
    let mut hidden = entry("gpt-hidden", 2, 0);
    hidden.as_object_mut().unwrap().remove("context_window");
    hidden["visibility"] = json!("hide");
    hidden["supported_reasoning_levels"] =
        json!([{ "effort": "minimal" }, { "effort": "medium" }, { "effort": "high" }]);
    hidden["default_reasoning_level"] = json!("medium");
    hidden["input_modalities"] = json!(["text"]);
    hidden["supports_search_tool"] = json!(false);
    let list = ModelList::parse(&wire(&[
        entry("gpt-tie", 2, 272_000),
        entry("not a slug", 0, 1),
        entry("gpt-first", 1, 500_000),
        hidden,
        entry("gpt-small", 3, 32_000),
    ]))
    .unwrap();
    let ids = ["gpt-first", "gpt-tie", "gpt-hidden", "gpt-small"];
    assert_eq!(list.ids(), ids);
    let small = list.find("gpt-small").unwrap();
    assert_eq!(small.window, None);
    let raw = |m: &ListedModel| (m.default_window, m.max_window);
    assert_eq!(raw(small), (Some(32_000), Some(872_000)));
    let first = list.find("gpt-first").unwrap();
    assert_eq!(raw(first), (Some(500_000), Some(872_000)));
    let facts = |m: &ListedModel| (m.shown, m.window, m.images, m.search);
    assert_eq!(facts(first), (true, Some(500_000), true, true));
    assert_eq!(first.levels(), Effort::ALL);
    assert_eq!(first.default_level(), Some(Effort::Low));
    let hidden = list.find("gpt-hidden").unwrap();
    assert_eq!(facts(hidden), (false, Some(872_000), false, false));
    assert_eq!(raw(hidden), (None, Some(872_000)));
    assert_eq!(hidden.levels(), [Effort::Medium, Effort::High]);
    assert_eq!(hidden.default_level(), Some(Effort::Medium));
    let saved = serde_json::to_string(&list).unwrap();
    assert!(!saved.contains("PLACEHOLDER"), "{saved}");
    let mut mistyped = entry("gpt-first", 1, 500_000);
    mistyped["context_window"] = json!("PLACEHOLDER");
    assert_eq!(ModelList::parse(&wire(&[mistyped])), Err(Unusable::Invalid));
    let fields = ["visibility", "input_modalities", "supports_search_tool"];
    for field in fields.into_iter().chain(["supported_reasoning_levels"]) {
        let mut missing = entry("gpt-first", 1, 500_000);
        missing.as_object_mut().unwrap().remove(field);
        assert_eq!(
            ModelList::parse(&wire(&[missing])),
            Err(Unusable::Invalid),
            "{field}"
        );
    }
    assert_eq!(
        ModelList::parse(&wire(&[entry("not a slug", 0, 1)])),
        Err(Unusable::Empty)
    );
}

#[test]
fn a_claude_list_shows_the_newest_known_model_of_each_line_and_keeps_only_the_fields_wisp_reads() {
    use Effort::{High, Low, Max, Medium};
    let off = json!({ "supported": false });
    let mut sonnet_4_6 = claude_entry("claude-sonnet-4-6", "sonnet", 1_000_000);
    sonnet_4_6["capabilities"]["effort"]["xhigh"] = Value::Null;
    let mut opus_4_5 = claude_entry("claude-opus-4-5-20251101", "opus", 200_000);
    opus_4_5["max_tokens"] = json!(64_000);
    opus_4_5["capabilities"]["effort"]["xhigh"] = off.clone();
    opus_4_5["capabilities"]["effort"]["max"] = off.clone();
    opus_4_5["capabilities"]["thinking"]["types"]["adaptive"] = off;
    let mut haiku_4_5 = claude_entry("claude-haiku-4-5-20251001", "haiku", 200_000);
    haiku_4_5["capabilities"]["effort"]["supported"] = json!(false);
    let mut deprecated = claude_entry("claude-sonnet-5", "sonnet", 1_000_000);
    deprecated["lifecycle"] = json!("deprecated");
    let mut lineless = claude_entry("claude-mythos-5", "mythos", 1_000_000);
    lineless["line"] = Value::Null;
    lineless["max_tokens"] = json!(0);
    let mut blank = claude_entry("claude-opus-5", "opus", 1_000_000);
    blank["capabilities"] = Value::Null;
    let small = 8_192;
    let mut unknown = claude_entry("claude-opus-6", "opus", 1_000_000);
    unknown["max_tokens"] = json!(small);
    let entries = [
        unknown.clone(),
        blank,
        claude_entry("claude-haiku-5-5", "haiku", 1_000_000),
        deprecated,
        claude_entry("claude-sonnet-5-5", "sonnet", 1_000_000),
        claude_entry("claude-opus-5-5", "opus", 1_000_000),
        claude_entry("claude-fable-5-1", "fable", 1_000_000),
        claude_entry("not a slug", "opus", 1_000_000),
        sonnet_4_6,
        opus_4_5,
        haiku_4_5,
        lineless,
    ];
    let list = anthropic::parse(&claude_wire(&entries)).unwrap();
    let shown = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
    let hidden = ["claude-sonnet-5", "claude-sonnet-4-6"];
    let dated = ["claude-opus-4-5-20251101", "claude-haiku-4-5-20251001"];
    let ids: [&[&str]; 6] = [
        &shown,
        &["claude-fable-5-1"],
        &["claude-opus-6"],
        &hidden,
        &dated,
        &["claude-mythos-5"],
    ];
    assert_eq!(list.ids(), ids.concat());
    let offers = list.offers();
    let ranked = offers.iter().filter(|offer| offer.listed.is_some()).count();
    assert_eq!(ranked, 5);
    let opus = list.find("claude-opus-5-5").unwrap();
    let facts = |m: &ListedModel| (m.window, m.max_output, m.adaptive, m.images, m.search);
    let full = (Some(1_000_000), Some(128_000), Some(true), true, true);
    assert_eq!(facts(opus), full);
    assert_eq!(opus.levels(), Effort::ALL);
    assert_eq!(opus.default_level(), None);
    let levels = |id: &str| list.find(id).unwrap().levels();
    assert_eq!(levels("claude-sonnet-4-6"), [Low, Medium, High, Max]);
    assert_eq!(levels("claude-opus-4-5"), [Low, Medium, High]);
    assert!(levels("claude-haiku-4-5").is_empty());
    let opus_4_5 = list.find("claude-opus-4-5").unwrap();
    assert_eq!(opus_4_5.id, "claude-opus-4-5-20251101");
    let short = (Some(200_000), Some(64_000), Some(false), true, true);
    assert_eq!(facts(opus_4_5), short);
    assert_eq!(list.find("claude-mythos-5").unwrap().max_output, None);
    assert_eq!(list.find("claude-opus-6").unwrap().max_output, Some(small));
    let caps =
        crate::provider::anthropic::capabilities::listed(Some(&list), "claude-opus-6").unwrap();
    assert_eq!(caps.max_output, Some(small));
    let saved = serde_json::to_string(&list).unwrap();
    assert!(!saved.contains("PLACEHOLDER"), "{saved}");
    let mut unknown = [unknown];
    let parse = |entries: &[Value]| anthropic::parse(&claude_wire(entries));
    assert_eq!(parse(&unknown).unwrap().ids(), ["claude-opus-6"]);
    unknown[0]["max_tokens"] = json!(0);
    assert_eq!(
        parse(&unknown)
            .unwrap()
            .find("claude-opus-6")
            .unwrap()
            .max_output,
        None
    );
    let mut mistyped = claude_entry("claude-opus-5-5", "opus", 1_000_000);
    mistyped["max_input_tokens"] = json!("PLACEHOLDER");
    assert_eq!(parse(&[mistyped]), Err(Unusable::Invalid));
    let mut missing = claude_entry("claude-opus-5-5", "opus", 1_000_000);
    missing["capabilities"]["image_input"] = Value::Null;
    assert_eq!(parse(&[missing]), Err(Unusable::Invalid));
}

#[test]
fn a_gap_keeps_both_sides_and_a_default_outside_the_levels_is_dropped() {
    use Effort::{High, Low, Max, Medium, Xhigh};
    let model = |levels: &[Effort], default_level| ListedModel {
        id: "gpt-x".into(),
        shown: true,
        window: None,
        default_window: None,
        max_window: None,
        levels: levels.to_vec(),
        default_level,
        max_output: None,
        adaptive: None,
        images: false,
        search: false,
    };
    assert_eq!(model(&[Low, High], Some(High)).levels(), [Low, High]);
    assert_eq!(model(&[Low, High], Some(Medium)).default_level(), None);
    assert_eq!(
        model(&[Max, Medium, Low], None).levels(),
        [Low, Medium, Max]
    );
    assert!(model(&[], Some(Low)).levels().is_empty());
    assert_eq!(model(&[Xhigh, Max], Some(Max)).default_level(), Some(Max));
}

#[test]
fn a_saved_list_round_trips_into_a_new_private_folder_and_goes_stale_after_an_hour() {
    #[derive(Deserialize)]
    struct PreviousSavedList {
        attempted: i64,
        list: ModelList,
        #[serde(default)]
        default_model: Option<String>,
    }
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("models");
    let saved = SavedList {
        attempted: 1_000,
        failed: false,
        list: sample(),
        default_model: None,
    };
    saved.save(&folder, KEY).unwrap();
    assert_eq!(SavedList::load(&folder, KEY), Some(saved.clone()));
    let file = folder.join(format!("{KEY}.json"));
    let read = || serde_json::from_slice::<Value>(&std::fs::read(&file).unwrap()).unwrap();
    let mut old = read();
    assert!(old.get("failed").is_none());
    let failed = SavedList {
        failed: true,
        ..saved.clone()
    };
    failed.save(&folder, KEY).unwrap();
    assert_eq!(SavedList::load(&folder, KEY), Some(failed));
    let previous: PreviousSavedList = record::read(&folder, KEY).unwrap();
    assert_eq!(previous.attempted, saved.attempted);
    assert_eq!(previous.list, saved.list);
    assert_eq!(previous.default_model, saved.default_model);
    assert_eq!(old["list"][0]["window"], 500_000);
    for entry in old["list"].as_array_mut().unwrap() {
        let entry = entry.as_object_mut().unwrap();
        assert!(entry.remove("default_window").is_some());
        assert!(entry.remove("max_window").is_some());
    }
    std::fs::write(&file, old.to_string()).unwrap();
    let loaded = SavedList::load(&folder, KEY).unwrap();
    let sol = loaded.list.find("gpt-6.1-sol").unwrap();
    assert_eq!(
        (sol.window, sol.default_window, sol.max_window),
        (Some(500_000), None, None)
    );
    loaded.save(&folder, KEY).unwrap();
    assert_eq!(read(), old);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&file), 0o600);
    }
    assert!(!loaded.failed);
    assert!(!loaded.stale(1_000 + REFRESH_AFTER - 1));
    assert!(loaded.stale(1_000 + REFRESH_AFTER));
    assert_eq!(SavedList::load(&folder, &KEY.replace('0', "f")), None);
    let empty = SavedList {
        attempted: 1_000,
        failed: false,
        list: ModelList::default(),
        default_model: None,
    };
    assert_eq!(empty.into_list(), None);
    assert_eq!(saved.into_list(), Some(sample()));
}

#[test]
fn a_sign_ins_key_keeps_its_saved_file_name_and_an_api_keys_comes_from_its_account() {
    use crate::provider::credential::SubscriptionKind;
    let url = "https://chatgpt.com/backend-api/codex/responses";
    let signed_in = Credential::Subscription {
        kind: SubscriptionKind::ChatGpt,
        secret: "synthetic-oauth".into(),
        profile: Some("synthetic-profile".into()),
    };
    let pinned = "972f99ea89c585c0db415df7d11b082d";
    assert_eq!(key(url, &signed_in, "work").as_deref(), Some(pinned));
    let api = |secret: &str| Credential::ApiKey(secret.into());
    let work = key(url, &api("synthetic-key"), "work").unwrap();
    assert_eq!(work, "af682bf8bd24db81c54d036f6e096e64");
    assert_eq!(
        key(url, &api("synthetic-other"), "work"),
        Some(work.clone())
    );
    assert_ne!(key(url, &api("synthetic-key"), "home"), Some(work));
    let setup = Credential::Subscription {
        kind: SubscriptionKind::Anthropic,
        secret: "synthetic-setup".into(),
        profile: None,
    };
    let bearer = Credential::AuthToken("synthetic-token".into());
    assert_eq!(key(url, &setup, "work"), None);
    assert_eq!(
        key(url, &bearer, "work"),
        key(url, &api("synthetic-key"), "work")
    );
    assert_ne!(key(url, &bearer, "work"), key(url, &bearer, "home"));
    let gateway = key("https://gw.example.com", &bearer, "work");
    assert_eq!(
        key(
            "https://gw.example.com/CANARY_path_token/v1/messages",
            &bearer,
            "work"
        ),
        gateway
    );
    assert_eq!(
        key("https://gw.example.com/other/v1/messages", &bearer, "work"),
        gateway
    );
    assert_ne!(
        key("https://gw.example.org/CANARY_path_token", &bearer, "work"),
        gateway
    );
}

#[test]
fn a_bad_id_in_a_saved_file_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let mut saved = SavedList {
        attempted: 1,
        failed: false,
        list: sample(),
        default_model: None,
    };
    saved.list.0[0].id = "not a slug".into();
    saved.save(dir.path(), KEY).unwrap();
    let loaded = SavedList::load(dir.path(), KEY).unwrap();
    assert_eq!(loaded.list.ids(), ["gpt-new"]);
}

#[test]
fn offers_rank_the_shown_entries_in_list_order_and_leave_the_hidden_unranked() {
    let mut hidden = entry("gpt-hidden", 1, 272_000);
    hidden["visibility"] = json!("hide");
    let list = ModelList::parse(&wire(&[
        entry("gpt-later", 2, 272_000),
        hidden,
        entry("gpt-first", 1, 500_000),
    ]))
    .unwrap();
    let offer = |id: &str, listed| AccountModel {
        id: id.to_owned(),
        listed,
    };
    assert_eq!(
        list.offers(),
        [
            offer("gpt-hidden", None),
            offer("gpt-first", Some(0)),
            offer("gpt-later", Some(1)),
        ]
    );
}

pub(crate) fn claude_list(entries: &[Value]) -> ModelList {
    anthropic::parse(&claude_wire(entries)).unwrap()
}

#[test]
fn a_newer_list_adds_only_what_the_cell_has_no_entry_for_and_nothing_a_connection_has_sent() {
    let dated = claude_entry("claude-opus-4-5-20251101", "opus", 200_000);
    let old = claude_list(&[claude_entry("claude-opus-5-5", "opus", 500_000), dated]);
    let mut changed = claude_entry("claude-opus-5-5", "opus", 1_000_000);
    changed["max_tokens"] = json!(64_000);
    let alias = claude_entry("claude-opus-4-5", "opus", 100_000);
    let rowless = claude_entry("claude-opus-6", "opus", 1_000_000);
    let haiku = claude_entry("claude-haiku-5-5", "haiku", 1_000_000);
    let dated_haiku = claude_entry("claude-haiku-4-5-20251001", "haiku", 200_000);
    let fresh = claude_list(&[rowless, changed, haiku, alias, dated_haiku]);
    let none = BTreeSet::new();
    let grown = old.with_new(&fresh, &none);
    let added = [
        "claude-haiku-5-5",
        "claude-opus-6",
        "claude-haiku-4-5-20251001",
    ];
    let with = |ids: &[&str]| [old.ids(), ids.iter().map(ToString::to_string).collect()].concat();
    assert_eq!(grown.ids(), with(&added));
    assert_eq!(grown.find("claude-opus-5-5"), old.find("claude-opus-5-5"));
    assert_eq!(grown.find("claude-opus-4-5"), old.find("claude-opus-4-5"));
    assert_eq!(old.with_new(&old, &none), old);
    let sent = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    let skipped = old.with_new(&fresh, &sent(&["claude-haiku-4-5", "claude-haiku-5-5"]));
    assert_eq!(skipped.ids(), with(&["claude-opus-6"]));
}

#[test]
fn a_cell_that_started_empty_stays_empty_and_one_that_found_nothing_new_keeps_its_list() {
    let old = Arc::new(claude_list(&[claude_entry(
        "claude-opus-5-5",
        "opus",
        500_000,
    )]));
    let fresh = claude_list(&[
        claude_entry("claude-opus-5-5", "opus", 500_000),
        claude_entry("claude-opus-6", "opus", 1_000_000),
    ]);
    let empty = ListCell::default();
    empty.add(&fresh);
    assert_eq!(empty.get(), None);
    let cell = ListCell::new(Some(old.clone()));
    cell.add(&old);
    assert!(Arc::ptr_eq(&cell.get().unwrap(), &old));
    cell.sent("claude-opus-6");
    cell.add(&fresh);
    assert!(Arc::ptr_eq(&cell.get().unwrap(), &old));
    let other = ListCell::new(Some(old));
    other.add(&fresh);
    assert_eq!(
        other.get().unwrap().ids(),
        ["claude-opus-5-5", "claude-opus-6"]
    );
}

#[test]
fn a_stale_list_is_claimed_by_one_cell_with_its_attempt_saved_first_and_a_fresh_one_only_read() {
    let dir = tempfile::tempdir().unwrap();
    let old = claude_list(&[claude_entry("claude-opus-5-5", "opus", 500_000)]);
    let newer = claude_list(&[
        claude_entry("claude-opus-5-5", "opus", 500_000),
        claude_entry("claude-opus-6", "opus", 1_000_000),
    ]);
    let cell = || ListCell::new(Some(Arc::new(old.clone())));
    let now = 10_000_000;
    let saved = |attempted, list: &ModelList| {
        let list = list.clone();
        SavedList {
            attempted,
            failed: false,
            list,
            default_model: None,
        }
        .save(dir.path(), KEY)
        .unwrap();
    };
    let attempt = || SavedList::load(dir.path(), KEY).unwrap().attempted;
    saved(now - REFRESH_AFTER, &old);
    let (first, second) = (cell(), cell());
    let busy = record::claim(dir.path(), KEY);
    assert!(second.begin(dir.path(), Some(KEY), false, now).is_none());
    assert_eq!(attempt(), now - REFRESH_AFTER);
    drop(busy);
    let claim = first.begin(dir.path(), Some(KEY), false, now);
    assert!(matches!(claim, Some(Claim::Held(_))));
    assert_eq!(attempt(), now);
    assert!(second.begin(dir.path(), Some(KEY), false, now).is_none());
    assert_eq!(second.get().unwrap().ids(), old.ids());
    saved(now + 1, &newer);
    drop(claim);
    assert!(
        second
            .begin(dir.path(), Some(KEY), false, now + 2)
            .is_none()
    );
    assert_eq!(second.get().unwrap().ids(), newer.ids());
    let due = now + 1 + REFRESH_AFTER;
    assert!(first.begin(dir.path(), Some(KEY), false, due).is_some());
    let held = record::claim(dir.path(), KEY);
    assert!(first.begin(dir.path(), Some(KEY), true, due + 1).is_some());
    assert_eq!(attempt(), due + 1);
    drop(held);
}

#[test]
fn gemini_ids_sort_listed_families_efforts_unlisted_and_unknown_deterministically() {
    use wisp_protocol::settings::{Chat, Landing, offered};

    let raw = [
        "gemini-exp-1206",
        "gemini-2.0-flash",
        "gemini-9-future-exp",
        "gemini-10-future",
        "gemini-3.1-flash-image",
        "gemini-2.5-flash",
        "gemini-3.6-flash",
        "gemini-3.8-flash-tiered",
        "gemini-2.5-pro",
        "gemini-3.8-flash-high",
        "gemini-4-hydrogen-xhigh",
        "gemini-4-hydrogen-low",
        "gemini-4-argon-high",
        "gemini-4-carbon-high",
        "gemini-4-carbon-low",
        "gemini-4-carbon-medium",
        "gemini-4-neon-xhigh",
        "gemini-4-neon-medium",
        "gemini-4-neon-high",
        "gemini-4-neon-low",
        "gemini-4-neon",
    ];
    let list = ModelList::from_gemini_ids(
        raw.iter().map(|id| ((*id).to_owned(), None, None)),
        Some("gemini-4-carbon-medium"),
    )
    .unwrap();
    let expected = [
        "gemini-4-neon",
        "gemini-4-neon-low",
        "gemini-4-neon-medium",
        "gemini-4-neon-high",
        "gemini-4-neon-xhigh",
        "gemini-4-carbon-low",
        "gemini-4-carbon-medium",
        "gemini-4-carbon-high",
        "gemini-4-argon-high",
        "gemini-4-hydrogen-low",
        "gemini-4-hydrogen-xhigh",
        "gemini-3.8-flash-high",
        "gemini-2.5-pro",
        "gemini-3.8-flash-tiered",
        "gemini-3.6-flash",
        "gemini-2.5-flash",
        "gemini-3.1-flash-image",
        "gemini-10-future",
        "gemini-9-future-exp",
        "gemini-2.0-flash",
        "gemini-exp-1206",
    ];
    assert_eq!(list.ids(), expected);
    let offers = list.offers();
    assert_eq!(
        offers
            .iter()
            .map(|m| (m.id.as_str(), m.listed))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, Some(u16::try_from(i).unwrap())))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        offered(
            &[],
            Landing::new("gemini", &offers, true, Some("gemini-4-carbon-medium")),
            Chat {
                provider: Some("gemini"),
                model: "gemini-4-carbon-medium",
            },
        ),
        expected
    );

    let mut reversed = list.clone();
    reversed.0.reverse();
    assert_eq!(
        reversed.sorted_gemini(Some("gemini-4-carbon-medium")).ids(),
        expected
    );

    for (default_id, want) in [
        (
            "gemini-5-preview",
            [
                "gemini-4-neon-high",
                "gemini-5-preview",
                "gemini-3.6-flash",
                "gemini-2.5-flash",
                "gemini-9-future-exp",
            ],
        ),
        (
            "gemini-2.5-flash",
            [
                "gemini-4-neon-high",
                "gemini-2.5-flash",
                "gemini-3.6-flash",
                "gemini-9-future-exp",
                "gemini-5-preview",
            ],
        ),
    ] {
        let with_default = ModelList::from_gemini_ids(
            [
                ("gemini-2.5-flash".into(), None, None),
                ("gemini-3.6-flash".into(), None, None),
                ("gemini-5-preview".into(), None, None),
                ("gemini-4-neon-high".into(), None, None),
                ("gemini-9-future-exp".into(), None, None),
            ],
            Some(default_id),
        )
        .unwrap();
        assert_eq!(with_default.ids(), want, "{default_id}");
    }
}
