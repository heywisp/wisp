use std::sync::Mutex;

use futures::executor::block_on;
use wisp_plugin_api::hooks::{CompactedInfo, RewindInfo};
use wisp_plugin_api::mcp::{
    Mcp as HostMcp, Prompt, PromptArgument, PromptMessage, Resource, Server,
};
use wisp_plugin_testkit::host::{FakeCall, FakeHost};

use super::*;

#[derive(Default)]
struct Service {
    servers: Mutex<Vec<Server>>,
    tools: Mutex<Vec<Tool>>,
    prompts: Mutex<Vec<Prompt>>,
    resources: Mutex<Vec<Resource>>,
    messages: Mutex<Vec<PromptMessage>>,
    parts: Mutex<Vec<Option<String>>>,
    read: Mutex<Vec<String>>,
    added: Mutex<Vec<(String, String, Vec<String>)>>,
    connected: Mutex<Vec<String>>,
    called: Mutex<Vec<(String, String)>>,
    revision: AtomicU64,
    sign_in_starts: AtomicU64,
    failure: Option<String>,
}

impl HostMcp for Service {
    fn prompts(&self) -> Vec<Prompt> {
        self.prompts.lock().unwrap().clone()
    }
    fn get_prompt<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PromptMessage>, String>> {
        Box::pin(async move {
            self.read.lock().unwrap().push(name.into());
            match &self.failure {
                Some(error) => Err(error.clone()),
                None => Ok(self.messages.lock().unwrap().clone()),
            }
        })
    }
    fn resources(&self) -> Vec<Resource> {
        self.resources.lock().unwrap().clone()
    }
    fn read_resource<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Option<String>>, String>> {
        Box::pin(async move {
            self.read.lock().unwrap().push(name.into());
            match &self.failure {
                Some(error) => Err(error.clone()),
                None => Ok(self.parts.lock().unwrap().clone()),
            }
        })
    }

    fn servers(&self) -> Vec<Server> {
        self.servers.lock().unwrap().clone()
    }
    fn tools(&self) -> Vec<Tool> {
        self.tools.lock().unwrap().clone()
    }
    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
    fn connect<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.connected.lock().unwrap().push(name.into());
            Ok(())
        })
    }
    fn call<'a>(
        &'a self,
        _: &'a dyn CallContext,
        name: &'a str,
        input: &'a str,
        _: u64,
    ) -> BoxFuture<'a, Result<ToolRun, String>> {
        Box::pin(async move {
            self.called
                .lock()
                .unwrap()
                .push((name.into(), input.into()));
            match &self.failure {
                Some(error) => Err(error.clone()),
                None => Ok(ToolRun::completed().with_result("called")),
            }
        })
    }
    fn add<'a>(
        &'a self,
        name: &'a str,
        command: &'a str,
        args: &'a [String],
    ) -> BoxFuture<'a, Result<u32, String>> {
        Box::pin(async move {
            self.added
                .lock()
                .unwrap()
                .push((name.into(), command.into(), args.to_vec()));
            match &self.failure {
                Some(error) => Err(error.clone()),
                None => Ok(3),
            }
        })
    }
    fn remove<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.servers
                .lock()
                .unwrap()
                .retain(|server| server.name != name);
            self.tools
                .lock()
                .unwrap()
                .retain(|tool| tool.server != name);
            self.prompts
                .lock()
                .unwrap()
                .retain(|prompt| prompt.server != name);
            self.resources
                .lock()
                .unwrap()
                .retain(|resource| resource.server != name);
            self.revision.fetch_add(1, Ordering::AcqRel);
            Ok(())
        })
    }
    fn sign_in_start<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<Option<String>, String>> {
        Box::pin(async move {
            self.sign_in_starts.fetch_add(1, Ordering::AcqRel);
            Ok(self
                .servers
                .lock()
                .unwrap()
                .iter()
                .any(|server| server.name == name && server.state == State::NeedsSignIn)
                .then(|| "https://mcp.example.invalid/authorize?placeholder".into()))
        })
    }
    fn sign_in_finish<'a>(
        &'a self,
        name: &'a str,
        _address: &'a str,
    ) -> BoxFuture<'a, Result<u32, String>> {
        Box::pin(async move {
            let mut servers = self.servers.lock().unwrap();
            let server = servers
                .iter_mut()
                .find(|server| server.name == name)
                .unwrap();
            server.state = State::Stopped;
            server.listed = true;
            self.revision.fetch_add(1, Ordering::AcqRel);
            Ok(server.tools)
        })
    }
}

fn tool() -> Tool {
    Tool {
        name: "mcp__local__echo".into(),
        server: "local".into(),
        original: "echo".into(),
        description: "Echo a message".into(),
        input_schema: r#"{"type":"object"}"#.into(),
    }
}

fn server(name: &str, state: State, listed: bool, tools: u32) -> Server {
    Server {
        name: name.into(),
        state,
        listed,
        off: false,
        tools,
    }
}

fn plugin(service: Arc<Service>, search: bool) -> Arc<dyn Plugin> {
    instantiate(Arc::new(FakeHost {
        mcp: Some(service),
        capabilities: if search {
            vec!["search".into()]
        } else {
            Vec::new()
        },
        ..FakeHost::default()
    }))
}

fn context() -> BuildContext {
    BuildContext {
        platform: "unix".into(),
        capabilities: vec!["search".into()],
    }
}

fn lines(reply: CommandReply) -> Vec<CommandLine> {
    let CommandReply::Lines(lines) = reply else {
        panic!("expected lines");
    };
    lines
}

fn plain(reply: CommandReply) -> String {
    lines(reply)
        .into_iter()
        .map(|line| {
            match line {
                CommandLine::Text(spans) => spans,
                CommandLine::Row(row) => [row.left, row.right].concat(),
            }
            .into_iter()
            .map(|span| span.text)
            .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn approved_server_states_share_their_tones() {
    assert_eq!(state(&State::Ready), ("running", Tone::Ok));
    assert_eq!(padded("本地", 6), "本地  ");
    assert_eq!(state(&State::Stopped), ("starts on use", Tone::Muted));
    assert_eq!(
        state(&State::Failed("failed".into())),
        ("failed", Tone::Error)
    );
}

#[test]
fn an_empty_list_is_the_approved_lines_answer_without_a_count() {
    let result = block_on(plugin(Arc::new(Service::default()), false).run_command("mcp", ""));
    assert_eq!(
        plain(result),
        "MCP servers\nnone yet: /mcp add <name> <command or address>"
    );
}

#[test]
fn rows_pad_names_align_counts_and_show_a_failed_servers_sentence() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![
            server("github", State::Ready, true, 41),
            server("files", State::Stopped, true, 12),
            server(
                "sentry",
                State::Failed("sentry couldn't start: npx isn't installed".into()),
                false,
                0,
            ),
        ]),
        ..Service::default()
    });
    let result = lines(block_on(plugin(service, true).run_command("mcp", "")));
    let CommandLine::Row(row) = &result[1] else {
        panic!("expected row");
    };
    assert_eq!(row.right[0].text, "41");
    assert_eq!(row.left[1].tone, Tone::Ok);
    let CommandLine::Row(row) = &result[2] else {
        panic!("expected row");
    };
    assert_eq!(row.left[0].text, "files   ");
    assert_eq!(row.left[1].tone, Tone::Muted);
    let CommandLine::Row(row) = &result[3] else {
        panic!("expected row");
    };
    assert_eq!(row.right[0].text, "—");
    assert_eq!(row.left[1].tone, Tone::Error);
    let CommandLine::Text(spans) = &result[4] else {
        panic!("expected sentence");
    };
    assert_eq!(
        spans[0].text,
        "        it couldn't start: npx isn't installed"
    );
}

#[test]
fn search_off_adds_the_approved_sentence_to_the_list_and_added_answer() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![server("local", State::Stopped, true, 3)]),
        ..Service::default()
    });
    let plugin = plugin(service, false);
    assert!(plain(block_on(plugin.run_command("mcp", ""))).ends_with(SEARCH_OFF));
    assert_eq!(
        plain(block_on(plugin.run_command("mcp", "add local server"))),
        format!("local added · 3 tools\n{SEARCH_OFF}")
    );
}

#[test]
fn shell_quotes_keep_an_argument_with_spaces_and_empty_arguments() {
    let service = Arc::new(Service::default());
    let plugin = plugin(service.clone(), true);
    assert_eq!(
        plain(block_on(plugin.run_command(
            "mcp",
            r#"add files server "~/My Documents" '' escaped\ word"#
        ))),
        "files added · 3 tools"
    );
    assert_eq!(
        *service.added.lock().unwrap(),
        [(
            "files".into(),
            "server".into(),
            vec!["~/My Documents".into(), "".into(), "escaped word".into()]
        )]
    );
}

#[test]
fn bad_syntax_does_not_call_the_service_or_echo_its_input() {
    let service = Arc::new(Service::default());
    let plugin = plugin(service.clone(), true);
    assert_eq!(
        plain(block_on(
            plugin.run_command("mcp", "add files server 'private")
        )),
        "the command has an unfinished quote"
    );
    assert_eq!(
        plain(block_on(plugin.run_command("mcp", "add files"))),
        "use /mcp add <name> <command>"
    );
    assert!(service.added.lock().unwrap().is_empty());
}

#[test]
fn a_refused_secret_is_not_repeated_in_the_lines_answer() {
    let service = Arc::new(Service {
        failure: Some("a key can't be saved here; when an agent adds the server with the key's name in keys, Wisp asks the user for it".into()),
        ..Service::default()
    });
    let result = plain(block_on(
        plugin(service, true)
            .run_command("mcp", "add notes server --token CANARY_NOT_A_REAL_SECRET"),
    ));
    assert_eq!(
        result,
        "notes not added: a key can't be saved here; when an agent adds the server with the key's name in keys, Wisp asks the user for it"
    );
    assert!(!result.contains("CANARY"));
}

#[test]
fn an_add_start_failure_uses_the_approved_sentence() {
    let service = Arc::new(Service {
        failure: Some("sentry couldn't start: npx isn't installed".into()),
        ..Service::default()
    });
    assert_eq!(
        plain(block_on(
            plugin(service, true).run_command("mcp", "add sentry npx -y server")
        )),
        "sentry not added: npx isn't installed"
    );
}

#[test]
fn dynamic_definitions_are_untrusted_network_tools_with_no_system_text() {
    let service = Arc::new(Service {
        tools: Mutex::new(vec![tool()]),
        ..Service::default()
    });
    let defs = block_on(plugin(service, true).describe(&context()));
    let ToolKind::Client(client) = &defs[0].kind else {
        panic!("expected client tool");
    };
    assert!(client.meta.tags.reads_untrusted && client.meta.tags.network);
    assert_eq!(client.meta.timeouts.max_ms, Some(TOTAL_MS));
    assert!(defs[0].system_line.is_none() && defs[0].context_line.is_none());
}

#[test]
fn a_prepared_call_has_the_original_name_and_forwards_input_and_result() {
    let service = Arc::new(Service {
        tools: Mutex::new(vec![tool()]),
        ..Service::default()
    });
    let plugin = plugin(service.clone(), true);
    block_on(plugin.describe(&context()));
    let call = block_on(plugin.prepare("mcp__local__echo", r#"{"text":"hello"}"#)).unwrap();
    assert_eq!(call.display().action, Action::Other("echo".into()));
    assert_eq!(call.display().target, "local");
    assert_eq!(
        block_on(plugin.run(call, &FakeCall::default()))
            .result
            .as_deref(),
        Some("called")
    );
    assert_eq!(
        *service.called.lock().unwrap(),
        [("mcp__local__echo".into(), r#"{"text":"hello"}"#.into())]
    );
    for failure in [None, Some(wording::CALL_CANCELLED.into())] {
        let service = Arc::new(Service {
            tools: Mutex::new(vec![tool()]),
            failure,
            ..Service::default()
        });
        let instance = self::plugin(service, true);
        block_on(instance.describe(&context()));
        let call = block_on(instance.prepare("mcp__local__echo", "{}")).unwrap();
        let context = FakeCall::default();
        context.cancel.set();
        assert_eq!(
            block_on(instance.run(call, &context)).outcome,
            wisp_plugin_api::types::Outcome::Cancelled
        );
    }
}

#[test]
fn a_removed_tools_routing_names_stay_without_retaining_its_definition() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![server("local", State::Ready, true, 1)]),
        tools: Mutex::new(vec![tool()]),
        failure: Some("local was removed".into()),
        ..Service::default()
    });
    let plugin = plugin(service.clone(), true);
    block_on(plugin.describe(&context()));
    service.tools.lock().unwrap().clear();
    assert_eq!(
        block_on(plugin.prepare("mcp__local__echo", "{}"))
            .err()
            .unwrap(),
        "local no longer has echo"
    );
    assert_eq!(
        plain(block_on(plugin.run_command("mcp", "remove local"))),
        "local removed"
    );
    assert!(block_on(plugin.describe(&context())).is_empty());
    let error = block_on(plugin.prepare("mcp__local__echo", "{}"))
        .err()
        .unwrap();
    assert_eq!(error, "local was removed");
    let display = block_on(plugin.rejected_display("mcp__local__echo", "{}")).unwrap();
    assert_eq!(display.target, "local");
}

#[test]
fn every_stopped_server_that_is_on_is_listed_by_the_background_hook() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![
            server("empty", State::Stopped, true, 0),
            server("missing", State::Stopped, false, 0),
            Server {
                off: true,
                ..server("off", State::Stopped, true, 0)
            },
            server("running", State::Ready, true, 3),
        ]),
        ..Service::default()
    });
    let plugin = plugin(service.clone(), true);
    block_on(plugin.on_hook(
        "list",
        Event::SessionStart(wisp_plugin_api::hooks::SessionStart { resumed: false }),
    ));
    block_on(plugin.on_hook(
        "list",
        Event::SessionStart(wisp_plugin_api::hooks::SessionStart { resumed: false }),
    ));
    assert_eq!(*service.connected.lock().unwrap(), ["empty", "missing"]);
}

#[test]
fn doctor_reports_states_and_counts_without_command_lines() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![
            server("local", State::Ready, true, 3),
            server(
                "missing",
                State::NeedsKey(vec!["SENTRY_TOKEN".into()]),
                true,
                9,
            ),
            server("failed", State::Failed("failed".into()), true, 9),
        ]),
        ..Service::default()
    });
    let rows = block_on(plugin(service, true).doctor(&context()));
    assert_eq!(rows[0].key.trim_end(), "mcp local");
    assert_eq!(
        rows[1].cells,
        [
            DoctorCell::Text("needs a key".into()),
            DoctorCell::Text("—".into())
        ]
    );
    assert_eq!(
        rows[2].cells,
        [
            DoctorCell::Text("failed".into()),
            DoctorCell::Text("—".into())
        ]
    );
    assert_eq!(
        rows[0].cells,
        [
            DoctorCell::Text("running".into()),
            DoctorCell::Text("3".into())
        ]
    );
}

#[test]
fn a_waiting_server_and_sign_in_answers_match_c2_c3_and_c4() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![server("linear", State::NeedsSignIn, false, 23)]),
        ..Service::default()
    });
    let plugin = plugin(service.clone(), true);
    let listed = lines(block_on(plugin.run_command("mcp", "")));
    let CommandLine::Row(row) = &listed[1] else {
        panic!("expected server")
    };
    assert_eq!(row.left[1].text, "needs sign-in");
    assert!(row.left[1].bold);
    assert_eq!(row.left[1].tone, Tone::Text);
    assert_eq!(row.right[0].text, "—");
    assert!(
        plain(block_on(plugin.run_command("mcp", ""))).ends_with("sign in: /mcp sign-in linear")
    );
    assert_eq!(
        plain(block_on(plugin.run_command(
            "mcp",
            "add linear https://mcp.example.invalid/mcp"
        ))),
        "linear added · sign in with /mcp sign-in linear"
    );
    assert_eq!(service.sign_in_starts.load(Ordering::Acquire), 0);
    let expected = "If the browser ends on a page that can't load, copy that page's address and type:\n/mcp sign-in linear <address>";
    assert_eq!(
        plain(block_on(plugin.run_command("mcp", "sign-in linear"))),
        expected
    );
    assert_eq!(service.sign_in_starts.load(Ordering::Acquire), 1);
    assert_eq!(
        plain(block_on(plugin.run_command("mcp", "sign-in linear"))),
        expected
    );
    assert_eq!(service.sign_in_starts.load(Ordering::Acquire), 2);
    assert_eq!(
        block_on(plugin.run_command(
            "mcp",
            "sign-in linear http://127.0.0.1:8123/oauth-callback?placeholder"
        )),
        CommandReply::None
    );
    assert_eq!(
        plain(block_on(plugin.run_command("mcp", "sign-in linear"))),
        "linear already signed in"
    );
    assert!(!plain(block_on(plugin.run_command("mcp", ""))).contains("sign in:"));
    assert_eq!(service.sign_in_starts.load(Ordering::Acquire), 3);
}

fn prompt(required: bool) -> Prompt {
    Prompt {
        name: "mcp__local__prompt:review.code".into(),
        server: "local".into(),
        original: "review.code".into(),
        description: "Review placeholder code\nSecond description line".into(),
        arguments: vec![PromptArgument {
            name: "path".into(),
            required,
            description: "The placeholder file".into(),
        }],
    }
}

fn resource() -> Resource {
    Resource {
        name: "mcp__local__resource:test:///notes".into(),
        server: "local".into(),
        uri: "test:///notes".into(),
        title: "Notes".into(),
        description: "Placeholder notes".into(),
    }
}

fn entries_service(required: bool) -> Arc<Service> {
    Arc::new(Service {
        servers: Mutex::new(vec![server("local", State::Stopped, true, 1)]),
        tools: Mutex::new(vec![tool()]),
        prompts: Mutex::new(vec![prompt(required)]),
        resources: Mutex::new(vec![resource()]),
        messages: Mutex::new(vec![
            PromptMessage {
                role: "user".into(),
                text: Some("Placeholder user text.".into()),
            },
            PromptMessage {
                role: "assistant".into(),
                text: Some("Placeholder assistant text.".into()),
            },
        ]),
        parts: Mutex::new(vec![Some("Placeholder document text.".into())]),
        ..Service::default()
    })
}

#[test]
fn saved_prompts_and_resources_become_distinct_searchable_entries_without_connecting() {
    let service = entries_service(false);
    assert!(block_on(plugin(service.clone(), false).entries()).is_empty());
    let mut other = resource();
    other.name = "mcp__local__resource:test:///other".into();
    other.uri = "test:///other".into();
    other.description.clear();
    service.resources.lock().unwrap().push(other);
    let plugin = plugin(service.clone(), true);
    let entries = block_on(plugin.entries());
    assert_eq!(
        entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
        [Kind::Skill, Kind::Document, Kind::Document]
    );
    assert_eq!(entries[0].summary, "Review placeholder code");
    assert!(entries[0].text.contains("path The placeholder file"));
    assert!(entries[0].text.contains("Second description line"));
    assert_eq!(entries[2].summary, "Notes");
    assert_ne!(entries[1].name, entries[2].name);
    assert!(service.connected.lock().unwrap().is_empty());
    assert!(service.read.lock().unwrap().is_empty());
    service.prompts.lock().unwrap().clear();
    assert_eq!(
        block_on(plugin.open_entry(&prompt(false).name)).unwrap_err(),
        "local no longer has review.code"
    );
    block_on(service.remove("local")).unwrap();
    assert_eq!(
        block_on(plugin.open_entry(&resource().name)).unwrap_err(),
        "local was removed"
    );
}

#[test]
fn prompt_text_keeps_roles_and_is_not_repeated_until_revision_rewind_or_compaction() {
    let service = entries_service(false);
    let plugin = plugin(service.clone(), true);
    let name = prompt(false).name;
    let first = block_on(plugin.open_entry(&name)).unwrap();
    assert_eq!(
        first,
        "Skill: mcp__local__prompt:review.code\nReview placeholder code\nSecond description line\nArguments:\n- path (optional): The placeholder file\n\nuser:\nPlaceholder user text.\n\nassistant:\nPlaceholder assistant text."
    );
    assert_eq!(
        block_on(plugin.open_entry(&name)).unwrap(),
        "Skill `mcp__local__prompt:review.code` is already in view."
    );
    assert_eq!(service.read.lock().unwrap().len(), 1);
    block_on(plugin.on_hook("rewound", Event::Rewound(RewindInfo { turn: 1, epoch: 1 })));
    assert_eq!(block_on(plugin.open_entry(&name)).unwrap(), first);
    block_on(plugin.on_hook(
        "compacted",
        Event::AfterCompact(CompactedInfo {
            epoch: 2,
            trigger: "auto".into(),
            engine: "summary".into(),
            before: 100,
            after: 50,
            summary: None,
            cost_micros: None,
            partial: false,
            failed: Some("placeholder failure".into()),
        }),
    ));
    assert_eq!(
        block_on(plugin.open_entry(&name)).unwrap(),
        "Skill `mcp__local__prompt:review.code` is already in view."
    );
    block_on(plugin.on_hook(
        "compacted",
        Event::AfterCompact(CompactedInfo {
            epoch: 3,
            trigger: "auto".into(),
            engine: "summary".into(),
            before: 100,
            after: 50,
            summary: None,
            cost_micros: None,
            partial: false,
            failed: None,
        }),
    ));
    assert_eq!(block_on(plugin.open_entry(&name)).unwrap(), first);
    service.revision.fetch_add(1, Ordering::AcqRel);
    assert_eq!(block_on(plugin.open_entry(&name)).unwrap(), first);
    assert_eq!(service.read.lock().unwrap().len(), 4);
    service.prompts.lock().unwrap()[0].arguments.clear();
    service.revision.fetch_add(1, Ordering::AcqRel);
    assert!(
        !block_on(plugin.open_entry(&name))
            .unwrap()
            .contains("Arguments:")
    );
}

#[test]
fn required_prompt_arguments_are_content_and_never_sent_to_get_prompt() {
    let service = entries_service(true);
    service.prompts.lock().unwrap()[0]
        .arguments
        .push(PromptArgument {
            name: "language".into(),
            required: false,
            description: String::new(),
        });
    let plugin = plugin(service.clone(), true);
    let text = block_on(plugin.open_entry(&prompt(true).name)).unwrap();
    assert_eq!(
        text,
        "Skill: mcp__local__prompt:review.code\nReview placeholder code\nSecond description line\nArguments:\n- path (required): The placeholder file\n- language (optional)\n\nuse can't pass these arguments, so the skill's text isn't shown."
    );
    assert!(service.read.lock().unwrap().is_empty());
    assert!(service.connected.lock().unwrap().is_empty());
}

#[test]
fn documents_reread_and_non_text_or_oversized_parts_return_no_partial_text() {
    let service = entries_service(false);
    let plugin = plugin(service.clone(), true);
    let name = resource().name;
    assert_eq!(
        block_on(plugin.open_entry(&name)).unwrap(),
        "Document: Notes\nURI: test:///notes\n\nPlaceholder document text."
    );
    service
        .parts
        .lock()
        .unwrap()
        .push(Some("Another placeholder part.".into()));
    assert!(
        block_on(plugin.open_entry(&name))
            .unwrap()
            .ends_with("\n\nAnother placeholder part.")
    );
    service.parts.lock().unwrap().push(None);
    assert_eq!(
        block_on(plugin.open_entry(&name)).unwrap_err(),
        "this document has parts that aren't text; use reads only text"
    );
    service.messages.lock().unwrap()[1].text = None;
    assert_eq!(
        block_on(plugin.open_entry(&prompt(false).name)).unwrap_err(),
        "this skill has parts that aren't text; use reads only text"
    );
    let limit = wording::MAX_ENTRY_BYTES;
    *service.parts.lock().unwrap() = vec![Some("x".repeat(limit))];
    assert_eq!(
        block_on(plugin.open_entry(&name)).unwrap_err(),
        "this document is 51 KiB; the limit is 50 KiB"
    );
    service.messages.lock().unwrap().truncate(1);
    service.messages.lock().unwrap()[0].text = Some("é".repeat(limit / 2));
    assert_eq!(
        block_on(plugin.open_entry(&prompt(false).name)).unwrap_err(),
        "this skill is 51 KiB; the limit is 50 KiB"
    );
    let header = "Document: Notes\nURI: test:///notes\n\n";
    *service.parts.lock().unwrap() = vec![Some("x".repeat(limit - header.len()))];
    assert_eq!(block_on(plugin.open_entry(&name)).unwrap().len(), limit);
}

#[test]
fn an_off_server_has_no_tools_prompts_or_resources_and_shows_where_to_turn_it_on() {
    let service = entries_service(false);
    service.servers.lock().unwrap()[0].off = true;
    let plugin = plugin(service.clone(), true);
    assert!(block_on(plugin.describe(&context())).is_empty());
    assert!(block_on(plugin.entries()).is_empty());
    let listing = lines(block_on(plugin.run_command("mcp", "")));
    let CommandLine::Row(row) = &listing[1] else {
        panic!("not a row");
    };
    assert_eq!(
        (row.left[1].text.as_str(), row.left[1].tone),
        ("off", Tone::Muted)
    );
    assert!(plain(CommandReply::Lines(listing)).contains("turn on: /config, Turned off"));
    service.servers.lock().unwrap()[0].off = false;
    assert_eq!(block_on(plugin.describe(&context())).len(), 1);
    assert_eq!(block_on(plugin.entries()).len(), 2);
}

#[test]
fn missing_keys_are_named_beside_sign_in_without_showing_a_count() {
    let service = Arc::new(Service {
        servers: Mutex::new(vec![
            server("linear", State::NeedsSignIn, false, 5),
            server(
                "sentry",
                State::NeedsKey(vec!["SENTRY_TOKEN".into(), "SENTRY_KEY".into()]),
                true,
                9,
            ),
        ]),
        ..Service::default()
    });
    let plugin = plugin(service, true);
    let listed = lines(block_on(plugin.run_command("mcp", "")));
    let CommandLine::Row(row) = &listed[2] else {
        panic!("expected server");
    };
    assert_eq!(row.left[1].text, "needs a key");
    assert!(row.left[1].bold);
    assert_eq!(row.left[1].tone, Tone::Text);
    assert_eq!(row.right[0].text, "—");
    let flat = plain(CommandReply::Lines(listed));
    assert!(flat.contains("sign in: /mcp sign-in linear"));
    assert!(flat.contains("set a key: ask your agent to set SENTRY_TOKEN and SENTRY_KEY for sentry; Wisp asks you for it"));
}
