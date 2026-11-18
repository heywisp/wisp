use super::*;
use crate::test_guard::dir;
use crate::test_guard::name;

fn endpoint(auth: AuthKind) -> NewEndpoint {
    NewEndpoint {
        provider: ProviderKind::Anthropic,
        auth,
        store: StoreKind::File,
        pricing: PricingMode::List,
    }
}

const COMMENTED: &str = "# my endpoints\ndefault = \"work\" # start here\n\n# the work gateway\n[endpoints.work]\nauth = \"bearer\"   # a token\nkey_env = \"WORK_KEY\"\nurl = \"https://gw.example.com\"\n\n[endpoints.old]\nauth = \"api_key\"\nkey_env = \"OLD\"\n";

#[test]
fn edits_keep_comments_blank_lines_and_key_order() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(text.existed());
    let id = StoreId::parse("5f0c2a9e41d7b388").unwrap();
    text.set_store_id(&id);
    text.add_endpoint(&name("direct"), &endpoint(AuthKind::ApiKey));
    text.set_default(Some(&name("direct")));
    text.write().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# my endpoints\ndefault = \"direct\" # start here\nstore_id = \"5f0c2a9e41d7b388\"\n\n# the work gateway\n[endpoints.work]\nauth = \"bearer\"   # a token\nkey_env = \"WORK_KEY\"\nurl = \"https://gw.example.com\"\n\n[endpoints.old]\nauth = \"api_key\"\nkey_env = \"OLD\"\n\n[endpoints.direct]\nauth = \"api_key\"\nkey_store = \"file\"\n"
    );
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(text.remove_endpoint(&name("old")));
    assert!(!text.remove_endpoint(&name("old")));
    text.set_default(None);
    text.write().unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.starts_with("# my endpoints\nstore_id"), "{after}");
    assert!(
        after.contains("# the work gateway\n[endpoints.work]\nauth = \"bearer\"   # a token\n")
    );
    assert!(!after.contains("old"), "{after}");
    let config = ConfigFile::parse(&path, PathShown::Default, &after).unwrap();
    assert_eq!(config.store_id, Some(id));
    assert_eq!(config.default, None);
}

#[test]
fn a_new_file_gets_a_plain_table_and_pricing_only_when_it_is_not_the_default() {
    let root = dir();
    let path = root.path().join("new/config.toml");
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(!text.existed());
    text.set_store_id(&StoreId::parse("0123456789abcdef").unwrap());
    text.add_endpoint(
        &name("gw"),
        &NewEndpoint {
            pricing: PricingMode::None,
            ..endpoint(AuthKind::Bearer)
        },
    );
    text.write().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "store_id = \"0123456789abcdef\"\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\npricing = \"none\"\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, NEW_MODE);
    }
}

#[test]
fn an_inline_endpoints_table_gets_an_inline_entry() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        "endpoints = { work = { auth = \"api_key\", key_env = \"W\" } }\n",
    )
    .unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    text.set_store_id(&StoreId::parse("0123456789abcdef").unwrap());
    text.add_endpoint(&name("direct"), &endpoint(AuthKind::ApiKey));
    text.write().unwrap();
    let config = ConfigText::read(&path, PathShown::Default)
        .unwrap()
        .current()
        .unwrap();
    assert_eq!(config.endpoints.len(), 2);
    assert_eq!(
        config.endpoints[&name("direct")].store(),
        Some(StoreKind::File)
    );
}

#[test]
fn a_file_that_does_not_parse_is_never_edited() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "[endpoints.x]\nkey = \"sk-ant-CANARY\"\n").unwrap();
    let error = ConfigText::read(&path, PathShown::Given).unwrap_err();
    assert!(
        matches!(error.problem, WriteProblem::Broken(_)),
        "{error:?}"
    );
    let words = error.to_string();
    assert!(
        words.starts_with("the file given with --config line 2: "),
        "{words}"
    );
    assert!(!words.contains("CANARY") && !words.contains(&path.display().to_string()));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[endpoints.x]\nkey = \"sk-ant-CANARY\"\n"
    );
}

#[test]
fn an_edit_that_would_not_parse_is_refused_before_writing() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    text.add_endpoint(&name("direct"), &endpoint(AuthKind::ApiKey));
    let error = text.write().unwrap_err();
    assert!(
        matches!(error.problem, WriteProblem::Invalid(_)),
        "{error:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
}

#[cfg(unix)]
#[test]
fn a_write_through_a_link_keeps_the_link_and_the_targets_mode() {
    use std::os::unix::fs::PermissionsExt;
    let root = dir();
    std::fs::create_dir_all(root.path().join("dotfiles")).unwrap();
    let target = root.path().join("dotfiles/config.toml");
    std::fs::write(&target, "# dotfiles\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = root.path().join("config.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut text = ConfigText::read(&link, PathShown::Default).unwrap();
    text.set_default(Some(&name("env")));
    assert!(text.write().unwrap().is_empty());
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "# dotfiles\ndefault = \"env\"\n"
    );
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        lock_folder(&link),
        std::fs::canonicalize(root.path().join("dotfiles")).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn a_hard_linked_config_gets_the_note() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    std::fs::hard_link(&path, root.path().join("other.toml")).unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    text.set_default(Some(&name("env")));
    let notes = text.write().unwrap();
    assert_eq!(
        notes,
        [PathNote {
            kind: PathNoteKind::HardLinks,
            path: path.clone(),
            repo: None,
            shown: PathShown::Default,
        }]
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("other.toml")).unwrap(),
        ""
    );
}

#[test]
fn errors_and_texts_never_show_a_given_path_in_debug_or_display() {
    let root = dir();
    let folder = root.path().join("CANARYFOLDER");
    std::fs::create_dir_all(folder.join("config.toml")).unwrap();
    let error = ConfigText::read(&folder.join("config.toml"), PathShown::Given).unwrap_err();
    for shown in [
        format!("{error:?}"),
        format!("{error:#?}"),
        error.to_string(),
    ] {
        assert!(!shown.contains("CANARY"), "{shown}");
    }
    assert!(
        error
            .to_string()
            .starts_with("could not read the file given with --config: ")
    );
    let text = ConfigText::read(&folder.join("new.toml"), PathShown::Given).unwrap();
    assert!(!format!("{text:?}").contains("CANARY"));
    let note = WriteProblem::TargetNotWritable(Box::new(PathNote {
        kind: PathNoteKind::TargetNotWritable,
        path: folder.clone(),
        repo: None,
        shown: PathShown::Given,
    }));
    let error = ConfigWriteError {
        path: folder.join("config.toml"),
        shown: PathShown::Given,
        problem: note,
    };
    assert_eq!(
        error.to_string(),
        "the file given with --config (a link) points into a folder Wisp can't write"
    );
    assert!(!format!("{error:?}").contains("CANARY"));
}

#[test]
fn canonical_config_names_a_file_that_does_not_exist_yet() {
    let root = dir();
    let path = root.path().join("config.toml");
    assert_eq!(
        canonical_config(&path),
        Some(
            std::fs::canonicalize(root.path())
                .unwrap()
                .join("config.toml")
        )
    );
    assert_eq!(
        canonical_config(&root.path().join("missing/config.toml")),
        None
    );
}

#[cfg(unix)]
#[test]
fn a_link_to_a_missing_file_is_refused_before_any_write() {
    let root = dir();
    let link = root.path().join("config.toml");
    std::os::unix::fs::symlink(root.path().join("gone/config.toml"), &link).unwrap();
    let error = ConfigText::read(&link, PathShown::Default).unwrap_err();
    assert_eq!(error.problem, WriteProblem::DanglingLink);
    assert!(error.to_string().starts_with("could not write "), "{error}");
    let error = ConfigText::read(&link, PathShown::Given).unwrap_err();
    assert_eq!(
        error.to_string(),
        "could not write the file given with --config: it is a link to a file that doesn't exist; create that file or replace the link. Nothing was changed"
    );
    let error = crate::secrets::apply(
        &link,
        PathShown::Default,
        crate::secrets::Change::SetDefault { name: name("x") },
        &crate::secrets::keyring::Unreachable(crate::secrets::KeyringError::Unsupported),
        std::time::Duration::from_millis(300),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::secrets::ChangeError::Config(ConfigWriteError {
            problem: WriteProblem::DanglingLink,
            ..
        })
    ));
    assert!(!root.path().join(crate::secrets::CREDENTIALS).exists());
}

#[test]
fn leading_blank_lines_are_kept() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "\n\n# top\nstore_id = \"0123456789abcdef\"\n").unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    text.set_store_id(&StoreId::parse("5f0c2a9e41d7b388").unwrap());
    text.write().unwrap();
    assert_eq!(
        read_text(&path),
        "\n\n# top\nstore_id = \"5f0c2a9e41d7b388\"\n"
    );
}

fn read_text(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn a_rename_and_field_edits_keep_comments_and_order() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, COMMENTED).unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(text.rename_endpoint(&name("work"), &name("gateway")));
    assert!(!text.rename_endpoint(&name("work"), &name("other")));
    text.set_default(Some(&name("gateway")));
    assert!(text.set_field(&name("gateway"), "auth", Some("api_key")));
    assert!(text.set_field(&name("gateway"), "pricing", Some("none")));
    assert!(text.set_field(&name("old"), "key_env", Some("NEWER")));
    assert!(!text.set_field(&name("missing"), "auth", Some("api_key")));
    text.write().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# my endpoints\ndefault = \"gateway\" # start here\n\n# the work gateway\n[endpoints.gateway]\nauth = \"api_key\"   # a token\nkey_env = \"WORK_KEY\"\nurl = \"https://gw.example.com\"\npricing = \"none\"\n\n[endpoints.old]\nauth = \"api_key\"\nkey_env = \"NEWER\"\n"
    );
    let mut inline = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(inline.set_field(&name("old"), "key_env", None));
    assert!(inline.set_field(&name("old"), "key_store", Some("file")));
    assert!(inline.current().is_err(), "key_store needs a store ID");
    let mut keys_file = ConfigText::read(&path, PathShown::Default).unwrap();
    keys_file.set_default(Some(&name("claude-api")));
    keys_file.write().unwrap();
    assert!(
        read_text(&path).starts_with("# my endpoints\ndefault = \"env\" # start here\n"),
        "{}",
        read_text(&path)
    );
}

#[test]
fn a_replaced_field_takes_the_old_ones_place_and_comments() {
    let root = dir();
    let path = root.path().join("config.toml");
    let head = "store_id = \"0123456789abcdef\"\n\n";
    std::fs::write(
        &path,
        format!("{head}[endpoints.gw]\n# the key\nkey_env = \"GW_KEY\" # work\n# the url\nurl = \"https://gw.example.com\"\nauth = \"bearer\"\n\n[endpoints.other]\nauth = \"api_key\"\nkey_env = \"OTHER_KEY\"\nurl_env = \"OTHER_URL\"\n"),
    )
    .unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(text.replace_fields(&name("gw"), &["url"], "url_env", "GW_URL"));
    text.write().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!(
            "{head}[endpoints.gw]\n# the key\nkey_env = \"GW_KEY\" # work\n# the url\nurl_env = \"GW_URL\"\nauth = \"bearer\"\n\n[endpoints.other]\nauth = \"api_key\"\nkey_env = \"OTHER_KEY\"\nurl_env = \"OTHER_URL\"\n"
        )
    );
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(text.replace_fields(&name("gw"), &["key_env", "url_env"], "key_store", "file"));
    assert!(text.replace_fields(&name("other"), &["url_env"], "url", "https://a.example.com"));
    assert!(text.replace_fields(&name("other"), &[], "pricing", "none"));
    assert!(!text.replace_fields(
        &name("missing"),
        &["url_env"],
        "url",
        "https://a.example.com"
    ));
    text.write().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!(
            "{head}[endpoints.gw]\n# the key\n# the url\nkey_store = \"file\" # work\nauth = \"bearer\"\n\n[endpoints.other]\nauth = \"api_key\"\nkey_env = \"OTHER_KEY\"\nurl = \"https://a.example.com\"\npricing = \"none\"\n"
        )
    );
    std::fs::write(
        &path,
        "endpoints = { gw = { auth = \"bearer\", key_env = \"GW_KEY\", url = \"https://gw.example.com\", pricing = \"none\" } }\n",
    )
    .unwrap();
    let mut text = ConfigText::read(&path, PathShown::Default).unwrap();
    assert!(text.replace_fields(&name("gw"), &["url"], "url_env", "GW_URL"));
    text.write().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "endpoints = { gw = { auth = \"bearer\", key_env = \"GW_KEY\", url_env = \"GW_URL\", pricing = \"none\" } }\n"
    );
}

fn target(text: &str) -> SitePath {
    match wisp_protocol::settings::parse_target(text).unwrap() {
        wisp_protocol::settings::Target::Name(spec) => SitePath::global(spec),
        wisp_protocol::settings::Target::Path(path) => path,
    }
}

fn set(text: &str, value: &str) -> SiteEdit {
    let path = target(text);
    let value = path.spec().parse(value).unwrap();
    SiteEdit::Set(path, value)
}

fn unset(text: &str) -> SiteEdit {
    SiteEdit::Unset(target(text))
}

fn edited(before: &str, edits: &[SiteEdit]) -> String {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, before).unwrap();
    save(&path, PathShown::Default, edits, Duration::from_secs(1)).unwrap();
    read_text(&path)
}

const SETTINGS_FILE: &str = "# my settings\neffort = \"high\" # thinking\n\n# the gateway\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\n\n# display\n[display]\ntheme = \"dark\"\n";

#[test]
fn a_write_of_a_renamed_section_drops_its_old_key() {
    let before = "effort = \"high\"\n\n[plugins.context.slots]\nusage = true\n\n[display]\ntheme = \"dark\"\n";
    assert_eq!(
        edited(before, &[set("plugins.context.slots.window", "off")]),
        "effort = \"high\"\n\n[plugins.context.slots]\nwindow = false\n\n[display]\ntheme = \"dark\"\n"
    );
    assert_eq!(
        edited(before, &[unset("plugins.context.slots.window")]),
        "effort = \"high\"\n\n[display]\ntheme = \"dark\"\n"
    );
}

#[test]
fn a_set_and_an_unset_keep_comments_layout_and_order() {
    let after = edited(
        SETTINGS_FILE,
        &[
            set("effort", "low"),
            set("display.mouse", "off"),
            set("endpoints.gw.pricing", "none"),
            set("model", "claude-sonnet-5-5"),
        ],
    );
    assert_eq!(
        after,
        "# my settings\neffort = \"low\" # thinking\nmodel = \"claude-sonnet-5-5\"\n\n# the gateway\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\npricing = \"none\"\n\n# display\n[display]\ntheme = \"dark\"\nmouse = false\n"
    );
    let back = edited(
        &after,
        &[
            unset("model"),
            unset("display.mouse"),
            unset("endpoints.gw.pricing"),
            unset("effort"),
        ],
    );
    assert!(back.contains("# my settings"), "{back}");
    assert!(back.contains("# the gateway"), "{back}");
    assert!(
        back.contains("# display\n[display]\ntheme = \"dark\"\n"),
        "{back}"
    );
    assert!(!back.contains("effort"), "{back}");
    assert!(!back.contains("pricing"), "{back}");
    assert_eq!(
        back,
        "# my settings\n\n# the gateway\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\n\n# display\n[display]\ntheme = \"dark\"\n"
    );
    assert_eq!(
        edited(
            "# top\ndisplay.theme = \"dark\"\neffort = \"low\"\n",
            &[unset("display.theme")]
        ),
        "# top\neffort = \"low\"\n"
    );
    assert_eq!(
        edited(
            "# header\neffort = \"high\"\n\n[display]\ntheme = \"dark\"\n",
            &[unset("effort")]
        ),
        "# header\n\n[display]\ntheme = \"dark\"\n"
    );
    assert_eq!(
        edited(
            "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"   # the gateway key\npricing = \"none\" \n",
            &[unset("endpoints.gw.pricing")]
        ),
        "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW_KEY\"   # the gateway key\n"
    );
}

#[test]
fn a_new_table_goes_at_the_end_and_an_emptied_one_is_removed() {
    let after = edited("effort = \"high\"\n", &[set("display.theme", "light")]);
    assert_eq!(after, "effort = \"high\"\n\n[display]\ntheme = \"light\"\n");
    let after = edited(&after, &[unset("display.theme")]);
    assert_eq!(after, "effort = \"high\"\n");
    let kept = edited(
        "[display] # mine\ntheme = \"light\"\n",
        &[unset("display.theme")],
    );
    assert_eq!(kept, "[display] # mine\n");
    let other = edited(
        "[display]\ntheme = \"light\"\nmouse = true\n",
        &[unset("display.theme")],
    );
    assert_eq!(other, "[display]\nmouse = true\n");
    let model = edited("", &[set("models.\"claude-sonnet-5.5\".compact_at", "60%")]);
    assert_eq!(
        model,
        "[models.\"claude-sonnet-5.5\"]\ncompact_at = \"60%\"\n"
    );
    assert_eq!(
        edited(&model, &[unset("models.\"claude-sonnet-5.5\".compact_at")]),
        ""
    );
    let keys_file = edited(
        "",
        &[
            set("endpoints.claude-api.native_web_search", "off"),
            set("endpoints.gateway.models.\"m\".compact_at", "60%"),
        ],
    );
    assert_eq!(
        keys_file,
        "[endpoints.env]\nnative_web_search = false\n\n[endpoints.env.models.m]\ncompact_at = \"60%\"\n"
    );
    let named = "[endpoints.claude-api]\nauth = \"api_key\"\nkey_env = \"K\"\n";
    assert_eq!(
        edited(
            named,
            &[set("endpoints.claude-api.native_web_search", "off")]
        ),
        format!("{named}native_web_search = false\n")
    );
}

#[test]
fn dotted_keys_and_inline_tables_keep_their_form() {
    let dotted = edited(
        "display.theme = \"dark\"\neffort = \"low\"\n",
        &[set("display.mouse", "off")],
    );
    assert_eq!(
        dotted,
        "display.theme = \"dark\"\ndisplay.mouse = false\neffort = \"low\"\n"
    );
    assert_eq!(
        edited(&dotted, &[unset("display.theme"), unset("display.mouse")]),
        "effort = \"low\"\n"
    );
    let inline = edited(
        "models = { \"m\" = { compact_at = \"60%\" } }\n",
        &[set("models.\"m\".context_window", "200k")],
    );
    assert_eq!(
        inline,
        "models = { \"m\" = { compact_at = \"60%\", context_window = 200000 } }\n"
    );
    assert_eq!(
        edited(
            &inline,
            &[
                unset("models.\"m\".compact_at"),
                unset("models.\"m\".context_window")
            ]
        ),
        ""
    );
}

#[test]
fn a_nested_inline_endpoint_keeps_its_spacing() {
    let after = edited(
        "endpoints = { gw = { auth = \"bearer\", key_env = \"GW_KEY\" } }\n",
        &[
            set("endpoints.gw.pricing", "none"),
            set("endpoints.gw.models.\"m\".compact_at", "60%"),
        ],
    );
    assert_eq!(
        after,
        "endpoints = { gw = { auth = \"bearer\", key_env = \"GW_KEY\", pricing = \"none\", models = { m = { compact_at = \"60%\" } } } }\n"
    );
}

#[test]
fn an_inline_unset_keeps_the_space_before_the_brace() {
    let before =
        "endpoints = { gw = { auth = \"bearer\", key_env = \"GW_KEY\", pricing = \"none\" } }\n";
    assert_eq!(
        edited(before, &[unset("endpoints.gw.pricing")]),
        "endpoints = { gw = { auth = \"bearer\", key_env = \"GW_KEY\" } }\n"
    );
}

#[test]
fn every_value_kind_is_written_in_its_toml_form() {
    let after = edited(
        "",
        &[
            set("debug_cache", "on"),
            set("instructions.warn_tokens", "8k"),
            set("retry.budget_usd", "$0.50"),
            set("tools", "bash"),
            set("display.scroll_speed", "3"),
        ],
    );
    assert_eq!(
        after,
        "debug_cache = true\ntools = [\"bash\"]\n\n[instructions]\nwarn_tokens = 8000\n\n[retry]\nbudget_usd = 0.5\n\n[display]\nscroll_speed = 3\n"
    );
    let parsed = ConfigFile::parse(Path::new("/h/c.toml"), PathShown::Default, &after).unwrap();
    assert_eq!(
        parsed.settings.global("retry.budget_usd"),
        Some(&SettingValue::Usd { micros: 500_000 })
    );
}

#[test]
fn the_endpoint_is_saved_as_default_and_secrets_are_never_written() {
    let base = "[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\n";
    let after = edited(base, &[set("endpoint", "gw")]);
    assert_eq!(after, format!("default = \"gw\"\n{base}"));
    assert_eq!(edited(&after, &[unset("default")]), base);
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, base).unwrap();
    for spec in ["key", "url", "store_id"] {
        let spec = wisp_protocol::settings::find(spec).unwrap();
        let place = if spec.name == "store_id" {
            wisp_protocol::settings::Place::Global
        } else {
            wisp_protocol::settings::Place::Endpoint("gw".into())
        };
        let edit = SiteEdit::Set(SitePath::new(place, spec), SettingValue::Text("x".into()));
        assert!(matches!(
            save(&path, PathShown::Default, &[edit], Duration::from_secs(1)),
            Err(SaveError::Refused(EditRefused::NotWritable(_)))
        ));
    }
    assert_eq!(read_text(&path), base);
}

#[test]
fn an_edit_that_would_not_parse_or_a_broken_file_writes_nothing() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        "default = \"gw\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"GW\"\n",
    )
    .unwrap();
    let before = read_text(&path);
    let error = save(
        &path,
        PathShown::Default,
        &[SiteEdit::Unset(target("endpoints.gw.auth"))],
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            SaveError::Write(ConfigWriteError {
                problem: WriteProblem::Invalid(_),
                ..
            })
        ),
        "{error:?}"
    );
    assert_eq!(read_text(&path), before);
    std::fs::write(&path, "effort = \n").unwrap();
    let error = save(
        &path,
        PathShown::Default,
        &[set("effort", "low")],
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            SaveError::Write(ConfigWriteError {
                problem: WriteProblem::Broken(_),
                ..
            })
        ),
        "{error:?}"
    );
    assert_eq!(read_text(&path), "effort = \n");
}

#[test]
fn a_held_lock_gives_try_again_and_changes_nothing() {
    let root = dir();
    let path = root.path().join("config.toml");
    std::fs::write(&path, "effort = \"high\"\n").unwrap();
    let held = crate::secrets::lock(
        &[(root.path(), LockKind::Config)],
        PathShown::Default,
        Duration::from_secs(1),
    )
    .unwrap();
    let error = save(
        &path,
        PathShown::Default,
        &[set("effort", "low")],
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert!(matches!(error, SaveError::Busy(_)));
    assert_eq!(
        error.to_string(),
        "config.toml wasn't changed: another wisp is changing it; try again."
    );
    drop(held);
    assert_eq!(read_text(&path), "effort = \"high\"\n");
    save(
        &path,
        PathShown::Default,
        &[set("effort", "low")],
        Duration::from_secs(1),
    )
    .unwrap();
    std::fs::write(&path, format!("{}model = \"m\"\n", read_text(&path))).unwrap();
    save(
        &path,
        PathShown::Default,
        &[set("time", "utc")],
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(
        read_text(&path),
        "effort = \"low\"\nmodel = \"m\"\ntime = \"utc\"\n"
    );
}

#[test]
fn a_settings_save_never_detaches_a_copied_config() {
    let root = dir();
    let path = root.path().join("config.toml");
    let text = "store_id = \"5f0c2a9e41d7b388\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\n";
    std::fs::write(&path, text).unwrap();
    let credentials = root.path().join("credentials.json");
    std::fs::write(&credentials, "{\"version\":1,\"stores\":{}}").unwrap();
    let saved = save(
        &path,
        PathShown::Default,
        &[set("endpoint", "gw"), set("endpoints.gw.pricing", "none")],
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(
        saved
            .notes
            .iter()
            .all(|n| n.kind != PathNoteKind::HardLinks)
    );
    assert_eq!(read_text(&credentials), "{\"version\":1,\"stores\":{}}");
    assert!(read_text(&path).contains("store_id = \"5f0c2a9e41d7b388\""));
}

#[test]
fn a_missing_given_folder_is_refused_and_a_missing_file_is_created() {
    let root = dir();
    let missing = root.path().join("nope").join("config.toml");
    assert_eq!(
        save(
            &missing,
            PathShown::Given,
            &[set("effort", "low")],
            Duration::from_secs(1)
        ),
        Err(SaveError::NoFolder)
    );
    assert!(!root.path().join("nope").exists());
    let path = root.path().join("config.toml");
    let saved = save(
        &path,
        PathShown::Given,
        &[set("effort", "low")],
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(saved.created);
    assert_eq!(read_text(&path), "effort = \"low\"\n");
}

#[test]
fn a_key_shaped_model_is_never_saved_by_value_or_by_table_name() {
    const CANARY: &str = "sk-ant-api03-CANARYMODEL0000";
    let root = dir();
    let path = root.path().join("config.toml");
    let base = "model = \"claude-opus-5-5\"\n";
    std::fs::write(&path, base).unwrap();
    let model = wisp_protocol::settings::find("model").unwrap();
    let compact = wisp_protocol::settings::find("compact_at").unwrap();
    let by_value = SiteEdit::Set(
        SitePath::new(Place::Global, model),
        SettingValue::Text(CANARY.into()),
    );
    let by_name = SiteEdit::Set(
        SitePath::new(Place::Model(CANARY.into()), compact),
        SettingValue::Text("60%".into()),
    );
    let fine = SiteEdit::Set(
        SitePath::new(Place::Global, model),
        SettingValue::Text("claude-sonnet-5-5".into()),
    );
    for edits in [vec![by_value], vec![fine.clone(), by_name]] {
        let refused = save(&path, PathShown::Default, &edits, Duration::from_secs(1));
        assert!(matches!(
            refused,
            Err(SaveError::Refused(EditRefused::KeyShapedModel))
        ));
        let words = refused.err().map(|e| e.to_string()).unwrap_or_default();
        assert_eq!(words, KEY_SHAPED_MODEL);
        assert!(!words.contains("CANARY"));
        assert_eq!(read_text(&path), base);
    }
    assert!(save(&path, PathShown::Default, &[fine], Duration::from_secs(1)).is_ok());
    assert!(read_text(&path).contains("claude-sonnet-5-5"));
}

#[test]
fn a_key_named_table_already_in_the_file_can_be_changed_but_never_created_or_a_placeholder() {
    const CANARY: &str = "sk-ant-api03-CANARYMODEL0000";
    let root = dir();
    let path = root.path().join("config.toml");
    let base = format!("[models.\"{CANARY}\"]\ncompact_at = \"60%\"\n");
    std::fs::write(&path, &base).unwrap();
    let compact = wisp_protocol::settings::find("compact_at").unwrap();
    let window = wisp_protocol::settings::find("context_window").unwrap();
    let wait = Duration::from_secs(1);
    let at = |place: Place, spec| SitePath::new(place, spec);
    let set = SiteEdit::Set(
        at(Place::Model(CANARY.into()), compact),
        SettingValue::Text("99%".into()),
    );
    assert!(save(&path, PathShown::Default, &[set], wait).is_ok());
    assert!(read_text(&path).contains("compact_at = \"99%\""));
    let added = SiteEdit::Set(
        at(Place::Model(CANARY.into()), window),
        SettingValue::Tokens(300_000),
    );
    assert!(save(&path, PathShown::Default, &[added], wait).is_ok());
    let unset = SiteEdit::Unset(at(Place::Model(CANARY.into()), compact));
    assert!(save(&path, PathShown::Default, &[unset], wait).is_ok());
    assert!(!read_text(&path).contains("compact_at"));
    let before = read_text(&path);
    let other = "sk-ant-api03-OTHERCANARY0000";
    let creates = SiteEdit::Set(
        at(
            Place::EndpointModel {
                endpoint: "gw".into(),
                model: CANARY.into(),
            },
            compact,
        ),
        SettingValue::Text("70%".into()),
    );
    let new_table = SiteEdit::Set(
        at(Place::Model(other.into()), compact),
        SettingValue::Text("70%".into()),
    );
    for edit in [creates, new_table] {
        let refused = save(&path, PathShown::Default, &[edit], wait);
        assert!(matches!(
            refused,
            Err(SaveError::Refused(EditRefused::KeyShapedModel))
        ));
        assert_eq!(read_text(&path), before);
    }
    let placeholder = SiteEdit::Set(
        at(Place::Model(wisp_protocol::NOT_SHOWN.into()), compact),
        SettingValue::Text("70%".into()),
    );
    let refused = save(&path, PathShown::Default, &[placeholder], wait);
    assert!(matches!(
        refused,
        Err(SaveError::Refused(EditRefused::NotAModelId))
    ));
    let words = refused.err().map(|e| e.to_string()).unwrap_or_default();
    assert_eq!(words, NOT_A_MODEL_ID_PLACE);
    assert_eq!(read_text(&path), before);
}

#[test]
fn a_value_beside_sub_tables_gets_its_own_header_and_leaves_none_behind() {
    let presets = "# presets\n[agents.presets.r]\ndescription = \"R\"\n";
    let after = edited(presets, &[set("agents.max_running", "2")]);
    assert_eq!(
        after,
        "[agents]\nmax_running = 2\n\n# presets\n[agents.presets.r]\ndescription = \"R\"\n"
    );
    assert_eq!(edited(&after, &[unset("agents.max_running")]), presets);
    let above = "model = \"m\"\n\n[agents.presets.r]\ndescription = \"R\"\n";
    let after = edited(above, &[set("agents.max_depth", "3")]);
    assert_eq!(
        after,
        "model = \"m\"\n\n[agents]\nmax_depth = 3\n\n[agents.presets.r]\ndescription = \"R\"\n"
    );
    assert_eq!(edited(&after, &[unset("agents.max_depth")]), above);
}

#[test]
fn a_declared_row_is_written_under_its_plugins_table() {
    use wisp_protocol::plugin::RowKind;
    crate::plugin::testing::declared(
        "written-row",
        RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    );
    let before = "[plugins.missing-row]\nold = 4\n";
    let after = edited(before, &[set("plugins.written-row.minutes", "25")]);
    assert!(
        after.contains("[plugins.written-row]\nminutes = 25"),
        "{after}"
    );
    assert!(after.contains(before), "{after}");
    let parsed = ConfigFile::parse(Path::new("config.toml"), PathShown::Default, &after).unwrap();
    assert!(parsed.settings.iter().any(|(path, saved)| path.shown()
        == "plugins.written-row.minutes"
        && saved.value == SettingValue::Int(25)));
    assert_eq!(
        edited(&after, &[unset("plugins.written-row.minutes")]),
        before
    );
}

#[test]
fn mcp_server_edits_round_trip_and_keep_unrelated_text() {
    let root = dir();
    let path = root.path().join("config.toml");
    let before = "# model\nmodel = \"claude-opus-5-5\" # keep\n\n[plugins.shell]\nenabled = true\n";
    std::fs::write(&path, before).unwrap();
    let server = crate::config::McpServer {
        keys: vec!["SENTRY_TOKEN".into()],
        command: "server".into(),
        args: vec!["a quoted argument".into(), "--stdio".into()],
    };
    let mut text = ConfigText::read(&path, PathShown::Given).unwrap();
    assert!(text.set_mcp_server("user.choice", &server));
    text.write().unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(before), "{after}");
    let mut text = ConfigText::read(&path, PathShown::Given).unwrap();
    assert_eq!(text.current().unwrap().mcp_servers["user.choice"], server);
    assert!(text.set_mcp_server(
        "second",
        &crate::config::McpServer {
            keys: Vec::new(),
            command: "another".into(),
            args: vec![]
        }
    ));
    assert!(text.remove_mcp_server("user.choice"));
    assert!(!text.remove_mcp_server("user.choice"));
    text.write().unwrap();
    let config = text.current().unwrap();
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(config.mcp_servers["second"].command, "another");
    for inline in [
        "mcp = {}",
        "mcp = { servers = {} }",
        "mcp = { servers = { second = {command = \"old\"} } }",
    ] {
        std::fs::write(&path, inline).unwrap();
        let mut text = ConfigText::read(&path, PathShown::Given).unwrap();
        assert!(text.set_mcp_server("second", &server));
        text.write().unwrap();
        assert_eq!(text.current().unwrap().mcp_servers["second"], server);
        assert!(text.remove_mcp_server("second"));
        text.write().unwrap();
        assert!(text.current().unwrap().mcp_servers.is_empty());
    }
}

#[test]
fn mcp_key_names_join_a_handwritten_table_without_rewriting_it() {
    let root = dir();
    let path = root.path().join("config.toml");
    let before = "# keep\n[mcp.servers.local]\ncommand = 'server' # user's command\nargs = ['--stdio']\nkeys = ['FIRST_TOKEN'] # user's names\n";
    std::fs::write(&path, before).unwrap();
    let mut text = ConfigText::read(&path, PathShown::Given).unwrap();
    assert!(text.add_mcp_keys("local", &["FIRST_TOKEN".into(), "SECOND_TOKEN".into()]));
    assert!(!text.add_mcp_keys("missing", &["SECOND_TOKEN".into()]));
    text.write().unwrap();
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(
        "# keep\n[mcp.servers.local]\ncommand = 'server' # user's command\nargs = ['--stdio']\n"
    ));
    assert!(after.contains("# user's names"));
    assert_eq!(
        text.current().unwrap().mcp_servers["local"].keys,
        ["FIRST_TOKEN", "SECOND_TOKEN"]
    );
}
