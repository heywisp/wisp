use super::registry::{
    AGENTS_GROUP, Applies, Form, NEW_CHATS, PRESETS_GROUP, SETTINGS, Secrecy, Section, SettingSpec,
    Site, Status, agents_group_keys, listed, ticked,
};
use crate::agents::{PRESET_FIELDS, preset_name_rule};
use crate::names::{ACCOUNT_FLAG, CONFIG_FILE};

fn saved_at(spec: &SettingSpec) -> String {
    let places: Vec<String> = spec
        .sites
        .iter()
        .filter(|s| **s != Site::Agent)
        .map(|site| match site {
            Site::Global => format!("`{}`", spec.file_key()),
            Site::Endpoint => format!("`endpoints.<e>.{}`", spec.leaf()),
            Site::Model => format!("`models.\"<m>\".{}`", spec.leaf()),
            Site::EndpointModel => format!("`endpoints.<e>.models.\"<m>\".{}`", spec.leaf()),
            Site::Shipped => "shipped".to_owned(),
            Site::Agent => String::new(),
            Site::PluginHook => "`[plugins.<id>.hooks]` `<hook>`".to_owned(),
            Site::PluginSlot => "`[plugins.<id>.slots]` `<name>`".to_owned(),
            Site::Plugin => format!(
                "`[plugins.<id>]` `{}`",
                spec.name.rsplit('.').next().unwrap_or_default()
            ),
        })
        .collect();
    places.join(", ")
}

fn command_line(spec: &SettingSpec) -> String {
    let mut parts = ticked(spec.flags);
    parts.extend(ticked(spec.env));
    match spec.secrecy {
        Secrecy::Plain if spec.applies == Applies::Connection => {
            parts.push("`wisp account edit`".to_owned());
        }
        Secrecy::Plain if spec.has_site(Site::Global) => {
            parts.push(format!("`--set {}=…`", spec.name));
        }
        Secrecy::Plain => parts.push("`--set PATH=…`".to_owned()),
        Secrecy::Secret => parts.push("`wisp account edit`".to_owned()),
        Secrecy::Hidden => parts.push("written by Wisp".to_owned()),
    }
    parts.join(", ")
}

pub fn render_docs() -> String {
    let mut out = format!(
        "# Settings\n\n\
         This page is generated from Wisp's settings registry (`wisp_protocol::settings`); a test keeps it in step, and `WISP_BLESS=1` rewrites it.\n\n\
         Every setting can be changed without editing `{CONFIG_FILE}`: `wisp config set NAME VALUE` saves it, `wisp config unset NAME` removes the saved value, and `wisp --set NAME=VALUE` (or a setting's own flag) sets it for one run. A setting saved per account or model takes its path, such as `wisp config set 'models.\"claude-opus-5-5\".compact_at' 80%`, or `wisp --set 'models.\"claude-opus-5-5\".compact_at=80%'` for one run, which applies at that place only and above the saved tables. `wisp config get NAME --explain` shows every place that sets a value and which one wins.\n\n\
         Where a value comes from, highest first: the session (the shortcuts `/model`, `/effort` and `/account`), flags and `--set`, the environment, the account\u{d7}model table, the model table, the account table, the top level of `{CONFIG_FILE}`, the shipped model facts, and the default. A `web_search` list that names only the other wire's versions is passed over (`docs/endpoints.md`, \"Web search\"). `/config` has one web search switch, the `Web search` row (the `web-search` plugin's own); it shows no row for `native_web_search` or a model's `web_search` list, which still act when saved: the row reads `off for this account` or `not on this model`, and `d` removes the saved value for {NEW_CHATS}.\n\n\
         `/config` starts with Model, Effort, Account, Web search, Tools, Compact at and Theme. Plugins opens the plugin list; More settings opens the remaining groups. Account opens the accounts, with their fields behind the right arrow; Context window stays on the model's page and Pricing on the account's page. `/config NAME` opens the page a setting is on, and `/` finds rows across every page.\n\n\
         Often used adds up to three rows from More settings or a plugin's page on the first screen: the most changed, then the latest, without reordering while the screen is open. Only an applied save of one of those rows on the settings screen counts, including when it is already under Often used. Unsaved changes, plugin switches, sidebar folds and its switch, `wisp config set` and model changes do not.\n\n\
         `settings-used.json`, beside the usage and models folders, holds only registered setting names, change counts and last-used day numbers, with the credentials file's permissions. Names unused for 60 days drop out; a missing or broken file reads as empty and the next use replaces it. These records reach no model request.\n\n\
         A row shows what this chat uses. `this chat` marks a value that differs from what {NEW_CHATS} use; the line under the row tells both values. Values set outside Wisp, by a mode or by a preset are named on that line instead, without a mark.\n\n\
         Saving Compact at, Context window, Background compaction or Background start on the settings screen removes that setting's value for this chat and ends its start flag in this chat, so the saved per-model numbers decide. In a number's box, Tab switches For between all models and this model; a third place appears only for a number already saved for this model on this account. Emptying the field and pressing Enter removes the number at the chosen place. On a `this chat` row, `d` returns the chat to what {NEW_CHATS} use without changing what is saved. It is not offered when a flag would keep winning or when returning to the current model's number would override other models; Enter still saves.\n\n\
         `?` opens the details at every width: This chat, Saved and Wisp's own values, the flag or variable, the file and line, and Name (the setting's name in the file). Numbers align on their right edge. A value saved only for {NEW_CHATS} keeps that mark when the screen is opened again. Tools and Plugins count the switches that are on; Compact at shows the point and the window the next turn uses, such as `850,000 of 1,000,000`.\n\n\
         On `--resume`, only the command line counts: a flag or `--set` value on the resume command applies to the resumed session from its next turn (`{ACCOUNT_FLAG}` from the start, `--instructions` and `--instructions-extra` as one rewrite of the project block), while config, environment and saved defaults never override what the session logged. A flag equal to the value the next turn would use anyway changes nothing; one that differs is named in the resume notice with what it does to the cache, and a key-shaped model is never shown there. Child agents keep their logged settings (`docs/session-log.md`, \"Flags on resume\").\n\n\
         A plugin can define on/off or whole-number settings under `[plugins.<id>]`. Where this Wisp has a plugin that defines settings, an unknown name is refused. Otherwise, a line that is not a table is kept and skipped; a near miss of `enabled` or `installed` is still refused.\n\n\
         Secrets (an account's key and URL) never pass through a setting command; `wisp config set` points to `wisp account edit`. `--set` doesn't change how an account connects (its provider, auth, key store or variables) for one run either, and points there too.\n\n\
         A path never echoes a key typed into it by mistake. A part that looks like a key (20 or more characters with a lowercase letter, a digit and no `_`) is refused as `that isn't a setting name`, unless it is the account part and names an account in `{CONFIG_FILE}`, such as `endpoints.prod-gateway-eu-west-2.pricing` (`--set` is read before `{CONFIG_FILE}`, so it refuses that account part too); a model ID of that shape is written in quotes. A model ID that looks like a key, with a run of 10 or more letters and digits holding both (such as `sk-ant-api03-\u{2026}`) or a run of 20 or more letters in both cases (such as Hugging Face's `hf_\u{2026}` and `api_org_\u{2026}`), is shown as `(looks like a key, not shown)` in `wisp config get` and `list` (with or without `--json`), `wisp doctor`, the start notices, the settings screen and the rest of the full screen, and is taken out of an error a provider sends back; Wisp still uses the value as given. A path under such a model, as saved in `{CONFIG_FILE}`, reads `models.\"MODEL\".\u{2026}` there, in what `wisp config set` and `unset` echo and in the settings screen's `wisp config set` line, and a sentence that names the model says `this model`. The session log and `--output jsonl` keep the value as given.\n",
    );
    out.push_str(&format!("\nMCP servers use named tables in `{CONFIG_FILE}`, separate from registered settings:\n\n```toml\n[mcp.servers.example]\ncommand = \"example-mcp-server\"\nargs = [\"--stdio\"]\n```\n\nThe kernel reads these tables without starting servers. `command` can also be an HTTP(S) address, with no arguments. Command arguments are passed directly. The optional `keys = [\"SENTRY_TOKEN\"]` names the environment variables the server reads; never put their values in this table. Wisp asks for missing keys in its secret box when an agent adds or updates a server, and keeps them in its secret store. An older Wisp refuses a settings file containing `keys`. HTTP sign-ins use the existing secret store, never this table. See `docs/mcp.md`.\n"));
    out.push_str("\n`preload` decides which permitted tools a new chat starts with: `[\"core\"]` (the default), `[\"none\"]` (only search and use), or a list of tool names. Followers come with their leader, search and use stay while on, and children keep report. Client tools left out are held and found by search; tools switched off cannot be found. `/config` calls the row **Start with** and offers core and none. A preset can name its own list; without one, a child uses the session's preload over its parent's permitted tools, including held ones.\n");
    for section in Section::ALL {
        let rows: Vec<&SettingSpec> = SETTINGS
            .iter()
            .filter(|s| s.section == section && s.is_built())
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {}\n\n", section.title()));
        out.push_str("| Setting | Values (default) | Saved at | Command line | Applies | |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        for spec in rows {
            out.push_str(&format!(
                "| `{}` | {} ({}) | {} | {} | {} | {} |\n",
                spec.name,
                spec.describe(Form::Docs),
                spec.default_words(),
                saved_at(spec),
                command_line(spec),
                spec.applies.words(),
                spec.help,
            ));
        }
    }
    out.push_str(&format!(
        "\n## Agent presets\n\n\
         Optional `[{PRESETS_GROUP}.NAME]` tables in `{CONFIG_FILE}` name the choices a spawn may make; `docs/agents.md` describes them. They live only in `{CONFIG_FILE}`: `wisp config set` points there. A name is {}. An unknown key, tool, model or effort, or instructions that don't load, turn off only that preset, and `wisp doctor` says why. An account that isn't set up or is unavailable stays in the preset for the spawn to resolve.\n\n",
        preset_name_rule()
    ));
    out.push_str("| Key | Values (default) | |\n|---|---|---|\n");
    for field in PRESET_FIELDS {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            field.key,
            field.values,
            field.help_text()
        ));
    }
    out.push_str("\n## Reserved\n\n");
    out.push_str("These names are kept for later; every command and the parser refuse them.\n\n");
    let taken = listed(&ticked(&agents_group_keys()), "and");
    out.push_str(&format!(
        "- `{AGENTS_GROUP}.<name>.*`, for any name other than {taken}: planned with agent types; a preset goes in `[{PRESETS_GROUP}.<name>]`.\n"
    ));
    for spec in SETTINGS {
        if let Status::Reserved { note } = spec.status {
            out.push_str(&format!("- `{}`: {note}.\n", spec.name));
        }
    }
    out
}
