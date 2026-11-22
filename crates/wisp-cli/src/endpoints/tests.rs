use std::path::{Path, PathBuf};

use wisp_core::endpoints::PathShown;
use wisp_core::secrets::{LockKind, PathNote, PathNoteKind};
use wisp_render::chrome::account::AccountKind;

use super::{Flags, Sources, doctor, note_words};

const ID: &str = "5f0c2a9e41d7b388";
const OTHER_ID: &str = "0123456789abcdef";
const THIRD_ID: &str = "aaaaaaaaaaaaaaaa";

struct Home {
    dir: crate::scratch::ScratchDir,
}

impl Home {
    fn new() -> Self {
        let dir = crate::scratch_dir("endpoints");
        dir.dir(".config/wisp");
        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.root.clone()
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    fn write(&self, relative: &str, text: &str) -> PathBuf {
        self.dir.write(relative, text)
    }

    fn doctor(&self, config: Option<&str>) -> String {
        self.doctor_with(&crate::keychain::Keychain::off(), config, None)
            .text
    }

    fn doctor_with(
        &self,
        keychain: &crate::keychain::Keychain,
        config: Option<&str>,
        env_file: Option<&str>,
    ) -> super::Report {
        let given = config.map(|c| self.path(c));
        let env_file = env_file.map(|e| self.path(e));
        let flags = Flags {
            env_file: env_file.as_deref(),
            config: given.as_deref(),
            endpoint: None,
            keychain,
            set: &[],
        };
        let sources = Sources::load(&flags, None, Some(self.root()));
        let home = self.root().display().to_string();
        let variables = crate::keychain::keychain(&sources.inputs);
        doctor(
            &sources,
            keychain,
            variables.as_ref().map_err(String::as_str),
            Some(&home),
        )
    }
}

fn config(store_id: Option<&str>, names: &[&str]) -> String {
    let mut text = store_id.map_or_else(String::new, |id| format!("store_id = \"{id}\"\n"));
    for name in names {
        text.push_str(&format!(
            "[endpoints.{name}]\nauth = \"api_key\"\nkey_store = \"file\"\n"
        ));
    }
    text
}

type Records<'a> = &'a [(&'a str, Option<&'a Path>)];

fn credentials(stores: &[(&str, Records<'_>)]) -> String {
    let stores: serde_json::Map<String, serde_json::Value> = stores
        .iter()
        .map(|(id, records)| {
            let records: serde_json::Map<String, serde_json::Value> = records
                .iter()
                .map(|(name, owner)| {
                    let mut record = serde_json::json!({"key": format!("sk-ant-CANARY-{name}")});
                    if let Some(owner) = owner {
                        record["owner"] = owner.display().to_string().into();
                    }
                    ((*name).to_owned(), record)
                })
                .collect();
            ((*id).to_owned(), serde_json::Value::Object(records))
        })
        .collect();
    serde_json::json!({"version": 1, "stores": stores}).to_string()
}

fn warning(text: &str, line: &str) -> bool {
    text.lines()
        .any(|l| l == format!("  {:<18} {line}", "warning"))
}

fn assert_warning(text: &str, line: &str) {
    assert!(warning(text, line), "missing {line:?} in\n{text}");
}

fn assert_clean(text: &str) {
    assert!(!text.contains("CANARY"), "{text}");
}

const COPY_HINT: &str = "its next change gives this config its own copy";
const ORPHAN_HINT: &str =
    "wisp account remove can't reach them, so delete them by hand or leave them";

#[test]
fn each_path_note_is_worded_for_both_origins() {
    let home = Some("/home/u");
    let note = |kind, shown| PathNote {
        kind,
        path: PathBuf::from("/home/u/dotfiles/wisp"),
        repo: Some(PathBuf::from("/home/u/dotfiles")),
        shown,
    };
    let readable = PathNote {
        path: PathBuf::from("/home/u/.config/wisp/credentials.json"),
        ..note(PathNoteKind::CredentialsReadable, PathShown::Default)
    };
    let cases = [
        (
            readable.clone(),
            "~/.config/wisp/credentials.json is readable by other users (chmod 600)",
        ),
        (
            PathNote {
                shown: PathShown::Given,
                ..readable
            },
            "credentials.json beside the file given with --config is readable by other users (chmod 600)",
        ),
        (
            note(
                PathNoteKind::CredentialsInRepo {
                    through_link: false,
                },
                PathShown::Default,
            ),
            "credentials.json is in ~/dotfiles/wisp, inside the git repository at ~/dotfiles; keep it out of commits with a .gitignore line",
        ),
        (
            note(
                PathNoteKind::CredentialsInRepo { through_link: true },
                PathShown::Default,
            ),
            "credentials.json is in ~/dotfiles/wisp, inside the git repository at ~/dotfiles (reached through a link); keep it out of commits with a .gitignore line",
        ),
        (
            note(
                PathNoteKind::CredentialsInRepo { through_link: true },
                PathShown::Given,
            ),
            "credentials.json beside the file given with --config is inside a git repository; keep it out of commits with a .gitignore line",
        ),
        (
            note(
                PathNoteKind::CredentialsInRepo {
                    through_link: false,
                },
                PathShown::Given,
            ),
            "credentials.json beside the file given with --config is inside a git repository; keep it out of commits with a .gitignore line",
        ),
        (
            note(
                PathNoteKind::LockInRepo(LockKind::Config),
                PathShown::Default,
            ),
            "config.toml's lock file .wisp.lock is in ~/dotfiles/wisp, inside the git repository at ~/dotfiles; it holds nothing, but add a .wisp.lock line to its .gitignore to keep it out of commits",
        ),
        (
            note(PathNoteKind::LockInRepo(LockKind::Config), PathShown::Given),
            "the lock file .wisp.lock beside the file given with --config (links resolved) is inside a git repository; it holds nothing, but add a .wisp.lock line to its .gitignore to keep it out of commits",
        ),
        (
            note(
                PathNoteKind::LockInRepo(LockKind::Credentials),
                PathShown::Default,
            ),
            "the lock file .wisp.lock beside credentials.json is in ~/dotfiles/wisp, inside the git repository at ~/dotfiles; it holds nothing, but add a .wisp.lock line to its .gitignore to keep it out of commits",
        ),
        (
            note(
                PathNoteKind::LockInRepo(LockKind::Credentials),
                PathShown::Given,
            ),
            "the lock file .wisp.lock beside credentials.json beside the file given with --config is inside a git repository; it holds nothing, but add a .wisp.lock line to its .gitignore to keep it out of commits",
        ),
        (
            note(PathNoteKind::CredentialsLinked, PathShown::Given),
            "credentials.json is a symbolic link; Wisp keeps keys only in a regular file",
        ),
        (
            note(PathNoteKind::TargetNotWritable, PathShown::Default),
            "~/dotfiles/wisp: Wisp can't write that folder",
        ),
        (
            note(PathNoteKind::TargetNotWritable, PathShown::Given),
            "the file given with --config (a link) points into a folder Wisp can't write",
        ),
        (
            note(PathNoteKind::HardLinks, PathShown::Default),
            "config.toml had other hard links; they still hold the old text",
        ),
        (
            note(PathNoteKind::CredentialsHardLinks, PathShown::Default),
            "~/dotfiles/wisp had other hard links; they still hold the old keys, including any removed one",
        ),
        (
            note(PathNoteKind::CredentialsHardLinks, PathShown::Given),
            "credentials.json beside the file given with --config had other hard links; they still hold the old keys, including any removed one",
        ),
    ];
    for (note, want) in cases {
        let words = note_words(&note, home);
        assert_eq!(words, want);
        if note.shown == PathShown::Given {
            assert!(
                !words.contains("/home/u") && !words.contains('~'),
                "{words}"
            );
        }
    }
}

#[test]
fn a_stored_endpoint_row_names_its_store_and_no_variable() {
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        &format!(
            "{}[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\n[endpoints.token]\nauth = \"anthropic_subscription\"\nkey_store = \"file\"\n",
            config(Some(ID), &["direct"])
        ),
    );
    home.write(
        ".config/wisp/credentials.json",
        &format!(
            "{{\"version\": 1, \"stores\": {{\"{ID}\": {{\"direct\": {{\"key\": \"sk-ant-CANARYKEY1\"}}, \"gw\": {{\"key\": \"CANARYTOKEN2\", \"url\": \"https://gw.example.com/CANARYPATH\"}}, \"token\": {{\"key\": \"sk-ant-oat01-CANARYTOKEN3\"}}}}}}}}"
        ),
    );
    let text = home.doctor(None);
    let rows = [
        "  credentials        ~/.config/wisp/credentials.json (3 keys)",
        &format!(
            "  direct             {} · API key in credentials.json \u{2192} api.anthropic.com · from ~/.config/wisp/credentials.json · list prices",
            AccountKind::ClaudeApi.label()
        ),
        "  gw                 Gateway · auth token and URL in credentials.json \u{2192} gw.example.com · from ~/.config/wisp/credentials.json · list prices",
        "  token              Claude subscription · key in credentials.json \u{2192} api.anthropic.com · from ~/.config/wisp/credentials.json · list prices",
    ];
    for row in rows {
        assert!(text.lines().any(|l| l == row), "missing {row:?} in\n{text}");
    }
    let shown: String = rows.concat();
    assert_eq!(
        wisp_render::chrome::account::vocabulary_slips(&shown),
        Vec::<&str>::new()
    );
    assert!(!text.contains("ANTHROPIC_API_KEY"), "{text}");
    assert_clean(&text);
}

#[test]
fn a_given_config_names_no_path_for_its_credentials() {
    let home = Home::new();
    home.write(
        "CANARYDIR/conf.toml",
        &config(Some(ID), &["direct", "lost"]),
    );
    let credentials = home.write(
        "CANARYDIR/credentials.json",
        &format!("{{\"version\": 1, \"stores\": {{\"{ID}\": {{\"direct\": {{\"key\": \"sk-ant-k\"}}}}}}}}"),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let text = home.doctor(Some("CANARYDIR/conf.toml"));
    let section = text.split("\naccounts\n").nth(1).unwrap_or_default();
    for row in [
        "  credentials        credentials.json beside the file given with --config (1 key)",
        &format!(
            "  direct             {} · API key in credentials.json \u{2192} api.anthropic.com · from credentials.json beside the file given with --config · list prices",
            AccountKind::ClaudeApi.label()
        ),
        "  lost               unavailable: no key for lost in credentials.json beside the file given with --config. If this config.toml was copied from another folder, its keys stayed in that folder's credentials.json: copy that file here too, or run wisp account edit lost --replace-key",
    ] {
        assert!(
            section.lines().any(|l| l == row),
            "missing {row:?} in\n{section}"
        );
    }
    #[cfg(unix)]
    assert_warning(
        section,
        "credentials.json beside the file given with --config is readable by other users (chmod 600)",
    );
    for line in section.lines().filter(|l| !l.starts_with("  config ")) {
        assert!(!line.contains("CANARYDIR"), "{line}");
    }
}

#[cfg(unix)]
#[test]
fn a_readable_default_credentials_file_is_named() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    home.write(".config/wisp/config.toml", &config(Some(ID), &["direct"]));
    let credentials = home.write(
        ".config/wisp/credentials.json",
        &format!("{{\"version\": 1, \"stores\": {{\"{ID}\": {{\"direct\": {{\"key\": \"sk-ant-k\"}}}}}}}}"),
    );
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o640)).unwrap();
    let text = home.doctor(None);
    assert_warning(
        &text,
        "~/.config/wisp/credentials.json is readable by other users (chmod 600)",
    );
}

#[test]
fn without_stored_keys_doctor_shows_no_credentials_row() {
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        "[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK\"\nmark = 9\n",
    );
    let text = home.doctor(None);
    assert!(!text.contains("credentials"), "{text}");
    assert!(
        !text.contains("error") && !text.contains('\u{25cf}'),
        "{text}"
    );
    assert_warning(&text, "work's mark isn't 1 to 6, so Wisp picks its colour");
    assert_eq!(text.matches("mark").count(), 1, "{text}");
}

#[test]
fn a_broken_credentials_file_is_shown_in_its_row() {
    let home = Home::new();
    home.write(".config/wisp/config.toml", &config(Some(ID), &["direct"]));
    home.write(
        ".config/wisp/credentials.json",
        "{\n\"version\": 1,\n\"stores\": CANARYBROKEN\n}",
    );
    let text = home.doctor(None);
    assert!(
        text.lines().any(|l| l
            == "  credentials        ~/.config/wisp/credentials.json (error: credentials.json line 3: not valid JSON)"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l
                == "  direct             unavailable: credentials.json line 3: not valid JSON"),
        "{text}"
    );
    assert_clean(&text);
}

#[test]
fn a_url_with_credentials_on_a_stored_endpoint_gets_the_stored_advice() {
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        &format!(
            "store_id = \"{ID}\"\n[endpoints.gw]\nauth = \"bearer\"\nkey_store = \"file\"\nurl = \"https://u:CANARYPASS@gw.example.com/x?CANARYQUERY\"\n"
        ),
    );
    home.write(
        ".config/wisp/credentials.json",
        &format!("{{\"version\": 1, \"stores\": {{\"{ID}\": {{\"gw\": {{\"key\": \"tok\"}}}}}}}}"),
    );
    let text = home.doctor(None);
    assert_warning(
        &text,
        "gw: the url holds user info or a query; store the URL with the key instead: run wisp account edit gw and change the URL",
    );
    assert_clean(&text);
}

#[test]
fn a_record_shared_with_the_default_config_says_so() {
    let home = Home::new();
    let default = home.write(".config/wisp/config.toml", &config(Some(ID), &["m"]));
    home.write(".config/wisp/copy.toml", &config(Some(ID), &["m"]));
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("m", Some(&default))])]),
    );
    let text = home.doctor(Some(".config/wisp/copy.toml"));
    assert_warning(
        &text,
        &format!(
            "m: shares its key with ~/.config/wisp/config.toml, which it was copied from; {COPY_HINT}"
        ),
    );
    let plain = home.doctor(None);
    assert!(!plain.contains("shares its key"), "{plain}");
}

#[test]
fn each_owner_state_gets_its_line() {
    let cases: [(&str, Option<&str>, &str); 5] = [
        (
            "other-same",
            Some(ID),
            "m: shares its key with another config file, which it was copied from",
        ),
        (
            "gone",
            None,
            "m: owned by another config file, which is gone",
        ),
        (
            "other-moved",
            Some(OTHER_ID),
            "m: owned by another config file, which no longer uses it",
        ),
        (
            "other-none",
            Some(""),
            "m: owned by another config file, which no longer uses it",
        ),
        (
            "other-broken",
            Some("broken"),
            "m: owned by another config file, which can't be read or doesn't parse right now",
        ),
    ];
    for (label, owner_store, line) in cases {
        let home = Home::new();
        home.write(".config/wisp/config.toml", &config(Some(ID), &["m"]));
        let owner = home.path(".config/wisp/owner.toml");
        match owner_store {
            None => {}
            Some("broken") => {
                home.write(".config/wisp/owner.toml", "store_id = [\n");
            }
            Some("") => {
                home.write(".config/wisp/owner.toml", "default = \"env\"\n");
            }
            Some(id) => {
                home.write(".config/wisp/owner.toml", &config(Some(id), &["m"]));
            }
        }
        home.write(
            ".config/wisp/credentials.json",
            &credentials(&[(ID, &[("m", Some(&owner))])]),
        );
        let text = home.doctor(None);
        assert_warning(&text, &format!("{line}; {COPY_HINT}"));
        assert!(!text.contains("owner.toml"), "{label}: {text}");
        assert_clean(&text);
    }
}

#[test]
fn the_default_config_as_owner_is_named_in_each_state() {
    let cases: [(Option<&str>, &str); 3] = [
        (
            Some(OTHER_ID),
            "m: owned by ~/.config/wisp/config.toml, which no longer uses it",
        ),
        (
            None,
            "m: owned by ~/.config/wisp/config.toml, which no longer uses it",
        ),
        (
            Some("broken"),
            "m: owned by ~/.config/wisp/config.toml, which can't be read or doesn't parse right now",
        ),
    ];
    for (default_store, line) in cases {
        let home = Home::new();
        let default = match default_store {
            Some("broken") => home.write(".config/wisp/config.toml", "store_id = [\n"),
            Some(id) => home.write(".config/wisp/config.toml", &config(Some(id), &[])),
            None => home.write(".config/wisp/config.toml", "default = \"env\"\n"),
        };
        home.write(".config/wisp/copy.toml", &config(Some(ID), &["m"]));
        home.write(
            ".config/wisp/credentials.json",
            &credentials(&[(ID, &[("m", Some(&default))])]),
        );
        let text = home.doctor(Some(".config/wisp/copy.toml"));
        assert_warning(&text, &format!("{line}; {COPY_HINT}"));
    }
}

#[test]
fn a_gone_default_owner_and_a_default_that_does_not_resolve_read_as_another_config() {
    let home = Home::new();
    let default = home.path(".config/wisp/config.toml");
    home.write(".config/wisp/copy.toml", &config(Some(ID), &["m"]));
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("m", Some(&default))])]),
    );
    let text = home.doctor(Some(".config/wisp/copy.toml"));
    assert_warning(
        &text,
        &format!("m: owned by another config file, which is gone; {COPY_HINT}"),
    );
    let elsewhere = home.write("other/owner.toml", &config(Some(ID), &["m"]));
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("m", Some(&elsewhere))])]),
    );
    let text = home.doctor(Some(".config/wisp/copy.toml"));
    assert_warning(
        &text,
        &format!(
            "m: shares its key with another config file, which it was copied from; {COPY_HINT}"
        ),
    );
}

#[test]
fn a_record_without_an_owner_reads_as_written_by_hand() {
    let home = Home::new();
    home.write(".config/wisp/config.toml", &config(Some(ID), &["m"]));
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("m", None)])]),
    );
    let text = home.doctor(None);
    assert_warning(&text, &format!("m: written by hand; {COPY_HINT}"));
    assert_clean(&text);
}

#[test]
fn orphans_are_grouped_by_owner_and_state() {
    let home = Home::new();
    let this = home.write(".config/wisp/config.toml", &config(Some(ID), &["m"]));
    let other = home.write("other/config.toml", &config(Some(OTHER_ID), &[]));
    let still = home.write("still/config.toml", &config(Some(THIRD_ID), &["s"]));
    let gone = home.path("gone/config.toml");
    let fourth = "bbbbbbbbbbbbbbbb";
    let fifth = "cccccccccccccccc";
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[
            (ID, &[("m", Some(&this)), ("old", Some(&this))]),
            (OTHER_ID, &[("a", None), ("b", None)]),
            (THIRD_ID, &[("s", Some(&still))]),
            (fourth, &[("c", Some(&gone)), ("d", Some(&gone))]),
            (
                fifth,
                &[("e", Some(&this)), ("f", Some(&this)), ("g", Some(&other))],
            ),
        ]),
    );
    let text = home.doctor(None);
    for line in [
        "credentials.json holds a key for \"old\", which config.toml doesn't name".to_owned(),
        "credentials.json holds 2 hand-written keys under a store this config doesn't use; another config in this folder may use them".to_owned(),
        format!("credentials.json holds 2 keys owned by another config file, which is gone; {ORPHAN_HINT}"),
        format!("credentials.json holds 2 keys owned by this config file, which no longer uses them; {ORPHAN_HINT}"),
        "credentials.json holds 1 key owned by another config file, which no longer uses it; wisp account remove can't reach it, so delete it by hand or leave it".to_owned(),
    ] {
        assert_warning(&text, &line);
    }
    assert!(!text.contains("\"s\""), "{text}");
    assert_eq!(text.matches("credentials.json holds").count(), 5, "{text}");
    assert_clean(&text);
}

#[test]
fn orphans_owned_by_the_default_or_this_given_config_are_named() {
    let home = Home::new();
    let default = home.write(".config/wisp/config.toml", &config(Some(ID), &[]));
    let given = home.write(".config/wisp/given.toml", &config(Some(OTHER_ID), &[]));
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[
            (THIRD_ID, &[("a", Some(&default))]),
            (
                "bbbbbbbbbbbbbbbb",
                &[("b", Some(&given)), ("c", Some(&given))],
            ),
        ]),
    );
    let text = home.doctor(Some(".config/wisp/given.toml"));
    assert_warning(
        &text,
        "credentials.json holds 1 key owned by ~/.config/wisp/config.toml, which no longer uses it; wisp account remove can't reach it, so delete it by hand or leave it",
    );
    assert_warning(
        &text,
        &format!(
            "credentials.json holds 2 keys owned by this config file, which no longer uses them; {ORPHAN_HINT}"
        ),
    );
    home.write(".config/wisp/config.toml", "store_id = [\n");
    let text = home.doctor(Some(".config/wisp/given.toml"));
    assert_warning(
        &text,
        "credentials.json holds 1 key owned by ~/.config/wisp/config.toml, which can't be read or doesn't parse right now, so doctor can't tell whether it is still used",
    );
}

#[test]
fn a_credentials_folder_in_a_git_work_tree_is_warned_about() {
    let home = Home::new();
    wisp_core::instructions::make_repository(&home.root()).unwrap();
    home.write(".config/wisp/config.toml", &config(Some(ID), &["m"]));
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("m", None)])]),
    );
    home.write(".config/wisp/.wisp.lock", "");
    let text = home.doctor(None);
    assert_warning(
        &text,
        "credentials.json is in ~/.config/wisp, inside the git repository at ~; keep it out of commits with a .gitignore line",
    );
    assert_warning(
        &text,
        "config.toml's lock file .wisp.lock is in ~/.config/wisp, inside the git repository at ~; it holds nothing, but add a .wisp.lock line to its .gitignore to keep it out of commits",
    );
    let given = home.doctor(Some(".config/wisp/config.toml"));
    assert_warning(
        &given,
        "credentials.json beside the file given with --config is inside a git repository; keep it out of commits with a .gitignore line",
    );
    assert_warning(
        &given,
        "the lock file .wisp.lock beside the file given with --config (links resolved) is inside a git repository; it holds nothing, but add a .wisp.lock line to its .gitignore to keep it out of commits",
    );
}

#[cfg(unix)]
#[test]
fn a_linked_credentials_file_makes_its_endpoints_unavailable() {
    let home = Home::new();
    home.write(".config/wisp/config.toml", &config(Some(ID), &["m"]));
    let target = home.write("elsewhere.json", &credentials(&[(ID, &[("m", None)])]));
    std::os::unix::fs::symlink(&target, home.path(".config/wisp/credentials.json")).unwrap();
    let text = home.doctor(None);
    assert!(
        text.lines().any(|l| l
            == "  m                  unavailable: credentials.json is a symbolic link; Wisp keeps keys only in a regular file"),
        "{text}"
    );
    assert_clean(&text);
}

#[cfg(unix)]
#[test]
fn a_linked_config_folder_in_a_repository_says_it_is_reached_through_a_link() {
    let home = Home::new();
    std::fs::remove_dir_all(home.path(".config/wisp")).unwrap();
    wisp_core::instructions::make_repository(&home.path("dotfiles")).unwrap();
    home.write("dotfiles/wisp/config.toml", &config(Some(ID), &["m"]));
    home.write(
        "dotfiles/wisp/credentials.json",
        &credentials(&[(ID, &[("m", None)])]),
    );
    std::os::unix::fs::symlink(home.path("dotfiles/wisp"), home.path(".config/wisp")).unwrap();
    let text = home.doctor(None);
    assert_warning(
        &text,
        "credentials.json is in ~/dotfiles/wisp, inside the git repository at ~/dotfiles (reached through a link); keep it out of commits with a .gitignore line",
    );
}

#[test]
fn a_record_for_an_endpoint_that_reads_a_variable_gets_its_own_line() {
    let home = Home::new();
    let text = format!(
        "{}[endpoints.work]\nauth = \"api_key\"\nkey_env = \"K\"\n",
        config(Some(ID), &["m"])
    );
    let this = home.write(".config/wisp/config.toml", &text);
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("m", Some(&this)), ("work", Some(&this))])]),
    );
    let text = home.doctor(None);
    assert_warning(
        &text,
        "credentials.json holds a key for \"work\"; config.toml names it but reads its key from a variable",
    );
    assert!(!text.contains("doesn't name"), "{text}");
    assert_clean(&text);
}

#[test]
fn doctor_lists_the_saved_settings_under_the_config_row() {
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        "effort = \"high\"\n\n[display]\ntheme = \"light\"\n\n[endpoints.gw]\nauth = \"bearer\"\nkey_env = \"canary0000000000000key1\"\n",
    );
    let text = home.doctor(None);
    assert!(
        text.contains("  settings           display.theme = light\n"),
        "{text}"
    );
    assert!(
        text.contains("                     effort = high\n"),
        "{text}"
    );
    assert!(!text.contains("canary0000000000000key1"), "{text}");
}

fn row_line(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with(&format!("  {key:<18} ")))
        .map(|l| l[21..].to_owned())
}

const KEYCHAIN_CONFIG: &str = "[endpoints.work]\nauth = \"bearer\"\nkey_env = \"WORK_KEY\"\nurl = \"https://u:CANARYPASS@gw.example.com/x\"\n";

fn keychain_home() -> Home {
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        &format!(
            "default = \"direct\"\n{}{KEYCHAIN_CONFIG}",
            config(Some(ID), &["direct"])
        ),
    );
    home.write(
        ".config/wisp/credentials.json",
        &credentials(&[(ID, &[("direct", None)])]),
    );
    home.write(
        ".config/wisp/.env",
        "ANTHROPIC_API_KEY=sk-ant-CANARYFILEKEY\nWORK_KEY=CANARYWORKKEY\n",
    );
    home
}

#[test]
fn on_a_keychain_machine_doctor_scans_the_env_file_for_names_only() {
    let home = keychain_home();
    let report = home.doctor_with(&crate::keychain::Keychain::available_down(), None, None);
    let text = &report.text;
    assert!(!report.broken, "{text}");
    assert_eq!(row_line(text, "keys file"), None, "{text}");
    assert_eq!(
        row_line(text, "would start on").as_deref(),
        Some("direct (default in config.toml)")
    );
    assert_eq!(
        row_line(text, "direct").as_deref(),
        Some(format!(
            "{} \u{b7} API key in credentials.json \u{2192} api.anthropic.com \u{b7} from ~/.config/wisp/credentials.json \u{b7} list prices \u{b7} a keychain is available: wisp account edit direct --store keyring moves it",
            AccountKind::ClaudeApi.label()
        ).as_str())
    );
    assert_eq!(
        row_line(text, "work").as_deref(),
        Some(
            "unavailable: its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
        )
    );
    assert_eq!(row_line(text, "claude-api"), None, "{text}");
    assert_warning(
        text,
        "~/.config/wisp/.env holds keys Wisp doesn't read on this machine",
    );
    assert_warning(
        text,
        "work: the url holds user info or a query; store the key and URL with wisp account import",
    );
    assert_clean(text);

    let off = home.doctor_with(&crate::keychain::Keychain::off(), None, None);
    assert_eq!(
        row_line(&off.text, "keys file").as_deref(),
        Some("~/.config/wisp/.env")
    );
    assert!(row_line(&off.text, "claude-api").is_some(), "{}", off.text);
    assert_warning(
        &off.text,
        "work: the url holds user info or a query; put credentials in ~/.config/wisp/.env and use url_env",
    );
    assert!(!off.text.contains("--store keyring moves it"));
    assert!(!off.text.contains("environment   "));
    assert_clean(&off.text);
}

#[test]
fn a_broken_env_file_is_a_warning_on_a_keychain_machine_and_an_error_without_one() {
    let home = keychain_home();
    home.write(
        ".config/wisp/.env",
        "ANTHROPIC_API_KEY=sk-ant-CANARYFILEKEY\nnot a line CANARYBROKEN\n",
    );
    let report = home.doctor_with(&crate::keychain::Keychain::available_down(), None, None);
    assert!(!report.broken, "{}", report.text);
    assert_warning(
        &report.text,
        "~/.config/wisp/.env doesn't parse (line 2); it isn't read on this machine",
    );
    assert_eq!(
        row_line(&report.text, "would start on").as_deref(),
        Some("direct (default in config.toml)")
    );
    assert_clean(&report.text);
    let off = home.doctor_with(&crate::keychain::Keychain::off(), None, None);
    assert!(off.broken);
    assert!(
        row_line(&off.text, "keys file").is_some_and(|l| l.starts_with("error: ")
            && l.ends_with("line 2 is not KEY=VALUE; nothing was loaded")),
        "{}",
        off.text
    );
    assert_clean(&off.text);
}

#[test]
fn a_given_env_file_is_named_but_not_read_on_a_keychain_machine() {
    let home = keychain_home();
    home.write("x.env", "ANTHROPIC_AUTH_TOKEN=CANARYGIVEN\n");
    let report = home.doctor_with(
        &crate::keychain::Keychain::available_down(),
        None,
        Some("x.env"),
    );
    assert_eq!(
        row_line(&report.text, "keys file").as_deref(),
        Some(
            "~/x.env (not read here: this machine has a keychain; wisp account import --env-file ~/x.env stores its keys)"
        )
    );
    assert!(!report.broken);
    assert_clean(&report.text);
}

#[test]
fn the_url_advice_follows_the_source_and_the_machine() {
    let name = wisp_core::endpoints::EndpointName::parse("gw").unwrap();
    assert_eq!(
        super::url_advice(&name, true, true, "~/.config/wisp/.env"),
        "store the URL with the key instead: run wisp account edit gw and change the URL"
    );
    assert_eq!(
        super::url_advice(&name, true, false, "~/.config/wisp/.env"),
        "store the URL with the key instead: run wisp account edit gw and change the URL"
    );
    assert_eq!(
        super::url_advice(&name, false, false, "~/.config/wisp/.env"),
        "put credentials in ~/.config/wisp/.env and use url_env"
    );
    assert_eq!(
        super::url_advice(&name, false, true, "~/.config/wisp/.env"),
        "store the key and URL with wisp account import"
    );
}

fn start_message(
    home: &Home,
    keychain: &crate::keychain::Keychain,
    endpoint: Option<&str>,
) -> String {
    let flags = Flags {
        env_file: None,
        config: None,
        endpoint,
        keychain,
        set: &[],
    };
    let sources = Sources::load(&flags, None, Some(home.root()));
    let failure = sources.resolve().err().unwrap();
    sources.start_failure(&failure)
}

#[test]
fn the_keychain_message_wins_only_without_a_stored_endpoint() {
    let on = crate::keychain::Keychain::available_down();
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        "default = \"work\"\n[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
    );
    home.write(
        ".config/wisp/.env",
        "ANTHROPIC_API_KEY=sk-ant-CANARYFILEKEY\n",
    );
    assert_eq!(
        start_message(&home, &on, None),
        "wisp: no account is set up yet. ~/.config/wisp/.env holds ANTHROPIC_API_KEY, but this machine has a keychain, so Wisp doesn't read it. Store it once with: wisp account import\nFor one run without the keychain: --keychain off"
    );
    assert_eq!(
        start_message(&home, &on, Some("work")),
        "wisp: account \"work\" is unavailable: its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
    );
    assert_eq!(
        start_message(&home, &on, Some("env")),
        "wisp: the keys-file account is unavailable: ~/.config/wisp/.env and the ANTHROPIC_* variables aren't read on a machine with a keychain; wisp account import --from-environment stores them"
    );

    home.write(
        ".config/wisp/config.toml",
        &format!(
            "default = \"direct\"\n{}[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
            config(Some(ID), &["direct"])
        ),
    );
    let generic = start_message(&home, &on, None);
    assert!(
        generic.starts_with(
            "wisp: the default account \"direct\" is unavailable: no key for direct in "
        ),
        "{generic}"
    );
    assert!(!generic.contains("CANARY"));

    let empty = Home::new();
    let off = crate::keychain::Keychain::off();
    assert!(
        start_message(&empty, &off, None)
            .starts_with("wisp: no Anthropic credentials: set ANTHROPIC_API_KEY"),
    );
    let exported = crate::presence::provider_keys()
        .iter()
        .any(|v| crate::presence::is_set(v.raw()));
    if exported {
        return;
    }
    assert_eq!(
        start_message(&empty, &on, None),
        "wisp: no account is set up yet. Run \"wisp setup\" in a terminal, or store a key with:\nprintenv VAR | wisp account add NAME --key-stdin"
    );
}

#[test]
fn an_injected_wisp_endpoint_keeps_its_own_reason_on_a_keychain_machine() {
    let on = crate::keychain::Keychain::available_down();
    let home = Home::new();
    home.write(
        ".config/wisp/config.toml",
        "[endpoints.work]\nauth = \"api_key\"\nkey_env = \"WORK_KEY\"\n",
    );
    home.write(".config/wisp/.env", "WORK_KEY=CANARYWORKKEY\n");
    super::TEST_CHOICE.with(|c| *c.borrow_mut() = Some(" work ".to_owned()));
    let message = start_message(&home, &on, None);
    super::TEST_CHOICE.with(|c| *c.borrow_mut() = None);
    assert_eq!(
        message,
        "wisp: account \"work\" is unavailable: its key is in the variable WORK_KEY, which Wisp doesn't read on a machine with a keychain; wisp account import stores it"
    );
    assert!(start_message(&home, &on, None).starts_with("wisp: no account is set up yet."));
}

#[test]
fn the_sources_apply_a_pricing_flag_to_every_book_they_resolve() {
    let home = Home::new();
    let config = home.write(
        "config.toml",
        "[endpoints.ok]\nauth = \"api_key\"\nkey_env = \"OK\"\n",
    );
    let env_file = home.write("keys.env", "OK=k\n");
    let keychain = crate::keychain::Keychain::off();
    let set =
        crate::settings::flag_layer(Vec::new(), &["endpoints.ok.pricing=none".to_owned()]).unwrap();
    for (given, priced) in [(&[][..], true), (&set[..], false)] {
        let flags = Flags {
            env_file: Some(&env_file),
            config: Some(&config),
            endpoint: None,
            keychain: &keychain,
            set: given,
        };
        let book = Sources::load(&flags, None, Some(home.root()))
            .book()
            .unwrap();
        assert_eq!(book.active.priced(), priced);
    }
}

#[test]
fn lists_are_due_when_missing_hourly_after_success_and_next_round_after_failure() {
    use wisp_core::endpoints::{
        Availability, EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, Unready, UrlSource,
    };
    use wisp_core::models::list::{ModelList, REFRESH_AFTER, SavedList};
    use wisp_core::provider::Provider;
    use wisp_core::provider::anthropic::DEFAULT_BASE_URL;
    use wisp_core::provider::credential::{Credential, Source, SubscriptionKind};
    use wisp_core::provider::openai::CHATGPT_BASE_URL;
    let dir = crate::scratch_dir("model-lists");
    let folder = dir.root.join("models");
    let ready = |name: &str, provider, credential, base_url: &str| {
        let endpoint = ResolvedEndpoint {
            client_version: None,
            name: EndpointName::parse(name).unwrap(),
            provider,
            credential,
            base_url: base_url.into(),
            source: Source::Keyring,
            key_env: None,
            url_source: UrlSource::Official,
            pricing: PricingMode::List,
        };
        (endpoint.name.clone(), Availability::Ready(endpoint))
    };
    let signed_in = |kind, profile: Option<&str>| Credential::Subscription {
        kind,
        secret: "fixture-token".into(),
        profile: profile.map(|file| dir.root.join(file)),
    };
    let account = |name: &str, profile: Option<&str>| {
        let credential = signed_in(SubscriptionKind::ChatGpt, profile);
        ready(name, ProviderKind::OpenAi, credential, CHATGPT_BASE_URL)
    };
    let claude = |name: &str, credential, base_url: &str| {
        ready(name, ProviderKind::Anthropic, credential, base_url)
    };
    let api_key = || Credential::ApiKey("fixture-key".into());
    let names = ["fresh", "missing", "old", "failed", "retry", "expired"];
    let mut entries: std::collections::BTreeMap<_, _> = names
        .iter()
        .map(|name| account(name, Some(&format!("{name}.json"))))
        .chain([account("token", None)])
        .chain([
            claude("claude-api", api_key(), DEFAULT_BASE_URL),
            claude("claude-gateway", api_key(), "https://gateway.example"),
            claude(
                "claude-plan",
                signed_in(SubscriptionKind::Anthropic, Some("claude.json")),
                DEFAULT_BASE_URL,
            ),
            claude(
                "claude-setup",
                signed_in(SubscriptionKind::Anthropic, None),
                DEFAULT_BASE_URL,
            ),
        ])
        .collect();
    let gone = Unready::new("signed out".into(), ProviderKind::OpenAi);
    entries.insert(
        EndpointName::parse("gone").unwrap(),
        Availability::Unavailable(gone),
    );
    wisp_core::secrets::write_atomic(&dir.root.join("expired.refused"), &[], 0o600).unwrap();
    let key = |name: &str| {
        let Some(Availability::Ready(endpoint)) = entries.get(&EndpointName::parse(name).unwrap())
        else {
            panic!("{name} is not ready");
        };
        let provider = endpoint.provider.connect(endpoint, None).unwrap();
        provider.list_key().map(str::to_owned)
    };
    for name in ["token", "claude-setup"] {
        assert_eq!(key(name), None, "{name}");
    }
    let now = 1_000_000;
    let near_next_round = super::ROUND_EVERY.as_secs().cast_signed() - 1;
    for (name, age, failed) in [
        ("fresh", near_next_round, false),
        ("old", REFRESH_AFTER, false),
        ("failed", 600, true),
        ("retry", near_next_round, true),
    ] {
        let list = ModelList::default();
        let saved = SavedList {
            attempted: now - age,
            failed,
            list,
            default_model: None,
            web_search_models: Vec::new(),
        };
        saved.save(&folder, &key(name).unwrap()).unwrap();
    }
    let due = super::due(&entries, &folder, now);
    let keys = |due: &[std::sync::Arc<dyn Provider>]| {
        due.iter()
            .map(|p| p.list_key().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&due.missing), [key("claude-plan"), key("missing")]);
    assert_eq!(
        keys(&due.stale),
        [
            key("claude-api"),
            key("claude-gateway"),
            key("old"),
            key("retry")
        ]
    );
    let listed = [
        "claude-api",
        "claude-gateway",
        "claude-plan",
        "failed",
        "fresh",
        "missing",
        "old",
        "retry",
    ];
    assert_eq!(due.listed, listed);
}

#[tokio::test(start_paused = true)]
async fn the_wait_for_missing_lists_ends_at_its_deadline_and_leaves_the_rest_running() {
    use futures::FutureExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    let finished = std::sync::Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let quick = async move { done.store(true, Ordering::SeqCst) }.boxed();
    let started = tokio::time::Instant::now();
    super::fetch_due(
        vec![std::future::pending().boxed(), quick],
        super::LIST_WAIT,
    )
    .await;
    assert_eq!(started.elapsed(), super::LIST_WAIT);
    assert!(finished.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn the_round_asks_each_listed_account_at_the_start_and_every_quarter_hour_after() {
    use futures::future::BoxFuture;
    use futures::stream::BoxStream;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wisp_core::agent::{Connection, LogContext};
    use wisp_core::endpoints::catalog::LiveCatalog;
    use wisp_core::kit::Connected;
    use wisp_core::provider::{Provider, ProviderError, ProviderEvent, Request};
    struct Listing(Arc<AtomicUsize>);
    impl Provider for Listing {
        fn stream(
            &self,
            _: &Request<'_>,
        ) -> BoxStream<'static, Result<ProviderEvent, ProviderError>> {
            Box::pin(futures::stream::empty())
        }
        fn cache_prefix(&self, _: &Request<'_>) -> Option<String> {
            None
        }
        fn list_key(&self) -> Option<&str> {
            Some("listing")
        }
        fn refresh_list(&self, _: PathBuf, always: bool) -> BoxFuture<'static, ()> {
            assert!(!always);
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
    }
    let asked = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn Provider> = Arc::new(Listing(asked.clone()));
    let work = Connection {
        name: "work".to_owned(),
        provider: provider.clone(),
        context: LogContext {
            endpoint: "https://api.anthropic.com".to_owned(),
            endpoint_name: "work".to_owned(),
            credential_source: wisp_protocol::log::CredentialSource::Environment,
            redactions: wisp_core::session::Redactions::default(),
        },
        priced: true,
    };
    let catalog = LiveCatalog::new(Connected {
        provider,
        ready: vec![work],
        unavailable: Vec::new(),
        active: "work".to_owned(),
        env_account: None,
    });
    let folder = crate::scratch_dir("list-rounds");
    let catalog = catalog.with_lists(Some(folder.root.clone()));
    super::keep_lists_fresh(catalog, vec!["work".to_owned(), "absent".to_owned()]);
    tokio::task::yield_now().await;
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    let quarter = super::ROUND_EVERY;
    tokio::time::sleep(quarter * 2 + std::time::Duration::from_secs(1)).await;
    assert_eq!(asked.load(Ordering::SeqCst), 3);
    assert_eq!(quarter * 4, std::time::Duration::from_secs(60 * 60));
}

#[test]
fn check_summary_derives_request_and_cost_by_provider_and_credential_without_slips() {
    use wisp_core::endpoints::{
        EndpointName, PricingMode, ProviderKind, ResolvedEndpoint, UrlSource,
    };
    use wisp_core::provider::anthropic::DEFAULT_BASE_URL;
    use wisp_core::provider::credential::{Credential, Source, SubscriptionKind};
    let make_ep = |name: &str, provider, credential, base_url: &str, url_source| ResolvedEndpoint {
        client_version: None,
        name: EndpointName::parse(name).unwrap(),
        provider,
        credential,
        base_url: base_url.into(),
        source: Source::Keyring,
        key_env: None,
        url_source,
        pricing: PricingMode::List,
    };
    let probe = |name: &str, provider, credential| {
        let ep = make_ep(
            name,
            provider,
            credential,
            "https://example.invalid",
            UrlSource::Official,
        );
        (name.to_owned(), super::Target::Probe(Box::new(ep)))
    };
    let sub = |kind| Credential::Subscription {
        kind,
        secret: "tok".into(),
        profile: None,
    };
    let api = || Credential::ApiKey("key".into());
    let skip = (
        "gone".to_owned(),
        super::Target::Skip {
            why: "missing".into(),
            fatal: false,
        },
    );

    assert_eq!(
        super::check_summary(std::slice::from_ref(&skip), "claude-opus-5-5"),
        (
            "count_tokens with claude-opus-5-5".to_owned(),
            "free on Anthropic's side; a gateway may count it".to_owned()
        )
    );
    assert_eq!(
        super::check_summary(
            &[probe(
                "ag",
                ProviderKind::Gemini,
                sub(SubscriptionKind::Antigravity)
            )],
            "gemini-4-carbon-medium"
        ),
        (
            "fetchAvailableModels".to_owned(),
            "included in your subscription".to_owned()
        )
    );
    assert_eq!(
        super::check_summary(
            &[probe("gapi", ProviderKind::Gemini, api())],
            "gemini-4-carbon"
        ),
        (
            "models list".to_owned(),
            "free on Google's side; a gateway may count it".to_owned()
        )
    );
    assert_eq!(
        super::check_summary(
            &[
                probe("cgpt", ProviderKind::OpenAi, sub(SubscriptionKind::ChatGpt)),
                probe(
                    "csub",
                    ProviderKind::Anthropic,
                    sub(SubscriptionKind::Anthropic)
                ),
            ],
            "gpt-6.1-sol"
        ),
        ("none".to_owned(), "none".to_owned())
    );
    assert_eq!(
        super::check_summary(&[probe("oapi", ProviderKind::OpenAi, api())], "gpt-6.1-sol"),
        (
            "models list".to_owned(),
            "free on OpenAI's side; a gateway may count it".to_owned()
        )
    );

    let official_claude = make_ep(
        "claude-api",
        ProviderKind::Anthropic,
        api(),
        DEFAULT_BASE_URL,
        UrlSource::Official,
    );
    let custom_claude = make_ep(
        "claude-gw",
        ProviderKind::Anthropic,
        api(),
        "https://gateway.example",
        UrlSource::Stored(wisp_core::secrets::StoreKind::Keyring),
    );
    let anthropic_default = ProviderKind::Anthropic.default_model();
    assert_eq!(
        super::anthropic_probe_model(&official_claude, "gemini-4-carbon"),
        anthropic_default
    );
    assert_eq!(
        super::anthropic_probe_model(&custom_claude, "gemini-4-carbon"),
        "gemini-4-carbon"
    );
    assert_eq!(
        super::check_summary(
            &[(
                "claude-api".to_owned(),
                super::Target::Probe(Box::new(official_claude))
            )],
            "gemini-4-carbon"
        ),
        (
            format!(
                "count_tokens with {}",
                wisp_protocol::shown_model(anthropic_default)
            ),
            "free on Anthropic's side; a gateway may count it".to_owned()
        )
    );
    assert_eq!(
        super::check_summary(
            &[(
                "claude-gw".to_owned(),
                super::Target::Probe(Box::new(custom_claude))
            )],
            "gemini-4-carbon"
        ),
        (
            "count_tokens with gemini-4-carbon".to_owned(),
            "free on Anthropic's side; a gateway may count it".to_owned()
        )
    );

    let mixed = vec![
        probe("oapi", ProviderKind::OpenAi, api()),
        probe("gapi", ProviderKind::Gemini, api()),
        probe(
            "ag",
            ProviderKind::Gemini,
            sub(SubscriptionKind::Antigravity),
        ),
        probe("cgpt", ProviderKind::OpenAi, sub(SubscriptionKind::ChatGpt)),
        probe(
            "csub",
            ProviderKind::Anthropic,
            sub(SubscriptionKind::Anthropic),
        ),
    ];
    let (req, cost) = super::check_summary(&mixed, "gemini-4-carbon");
    assert_eq!(req, "models list; fetchAvailableModels");
    assert_eq!(
        cost,
        "free on OpenAI's side; free on Google's side; included in your subscription; a gateway may count it"
    );
    for text in [req.as_str(), cost.as_str(), "none"] {
        assert_eq!(
            wisp_render::chrome::account::vocabulary_slips(text),
            Vec::<&str>::new(),
            "{text}"
        );
    }
}
