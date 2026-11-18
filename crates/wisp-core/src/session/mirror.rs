use wisp_protocol::AgentEvent;
use wisp_protocol::log::LogEvent;

macro_rules! mirror {
    (
        fn log_record($event:ident, $turn:ident);
        in_turn { $($t:ident { $($tf:ident),* })* }
        any_turn { $($m:ident { $($mf:ident),* })* }
        as_is { $($a:ident { $($af:ident),* })* }
        noted { $($n:ident { $($nf:ident),* })* }
        renamed { $($from:ident => $to:ident { $($rf:ident),* })* }
        logged_only { $($o:ident { $($of:ident),* })* }
        by_hand { $($hand:pat => $record:expr,)* }
        elsewhere { $($e:ident)* }
    ) => {
        pub(crate) fn log_record($event: &AgentEvent, $turn: Option<u64>) -> Option<LogEvent> {
            Some(match $event {
                $(AgentEvent::$t { $($tf),* } => LogEvent::$t {
                    turn: $turn?,
                    $($tf: $tf.clone()),*
                },)*
                $(AgentEvent::$m { $($mf),* } => LogEvent::$m {
                    turn: $turn,
                    $($mf: $mf.clone()),*
                },)*
                $(AgentEvent::$a { $($af),* } => LogEvent::$a { $($af: $af.clone()),* },)*
                $(AgentEvent::$n { $($nf,)* notes } => LogEvent::$n {
                    turn: $turn?,
                    $($nf: $nf.clone(),)*
                    notes: Some(notes.clone()),
                },)*
                $(AgentEvent::$from { $($rf),* } => LogEvent::$to {
                    turn: $turn?,
                    $($rf: $rf.clone()),*
                },)*
                $($hand => $record,)*
                $(AgentEvent::$o { $($of,)* .. } => LogEvent::$o {
                    turn: $turn,
                    $($of: $of.clone()),*
                },)*
                $(AgentEvent::$e { .. })|* => return None,
            })
        }

        pub(crate) fn live_event(record: &LogEvent) -> Option<AgentEvent> {
            Some(match record {
                $(LogEvent::$t { turn: _, $($tf),* } => AgentEvent::$t { $($tf: $tf.clone()),* },)*
                $(LogEvent::$m { turn: _, $($mf),* } => AgentEvent::$m { $($mf: $mf.clone()),* },)*
                $(LogEvent::$a { $($af),* } => AgentEvent::$a { $($af: $af.clone()),* },)*
                $(LogEvent::$n { turn: _, $($nf,)* notes } => AgentEvent::$n {
                    $($nf: $nf.clone(),)*
                    notes: notes.clone().unwrap_or_default(),
                },)*
                $(LogEvent::$to { turn: _, $($rf),* } => AgentEvent::$from {
                    $($rf: $rf.clone()),*
                },)*
                _ => return None,
            })
        }

        #[cfg(test)]
        pub(crate) const MIRRORED: usize = [
            $(stringify!($t),)* $(stringify!($m),)* $(stringify!($a),)*
            $(stringify!($n),)* $(stringify!($from),)*
        ]
        .len();
    };
}

mirror! {
    fn log_record(event, turn);
    in_turn {
        Fallback { from, to }
        ToolCallRequested { step, block, call_id, tool, input, display }
        ToolCallStarted { call_id, cwd, movable, file }
        QuestionAsked { call_id, questions }
        QuestionClosed { call_id, how }
        SearchStarted { step, attempt, call_id, query, action }
        Citations { step, attempt, index, sources }
        WakeHeld { sources, reason }
    }
    any_turn {
        InputHeld { ids, by }
        InputWithdrawn { ids, by }
        CacheMiss {
            previous_prompt_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            diagnostics,
            cause
        }
        CacheMissRepeated { billing, cause, count, usd }
        AgentRefused { by, action, reason }
        UnknownProviderEvent { provider, event }
        InputTransformations { provider, transformations }
        SearchEnded { step, attempt, call_id, outcome }
        EndpointsChanged { endpoints }
    }
    as_is {
        InstructionsLoaded { turn, chosen, imports, candidates, diagnostics }
        HookRan { hook, point, turn, step, call_id, outcome, ms }
        HookApplied { hook, point, turn, step, call_id, input_id, change }
        WindowLowered { endpoint, model, tokens }
        ProcessResized { id, size }
        ProcessEnded { id, end, bytes, lines, duration_ms }
        ProcessAttached { id, size }
        ProcessNeedsInput { id, kind }
        ProcessNeedsInputCleared { id }
    }
    noted {
        ToolCallFinished {
            call_id, outcome, output, facts, state, duration_ms, result, process, attachments, typed
        }
    }
    renamed {
        SearchNotRun => WebSearchNotRun { endpoint_name, model }
    }
    logged_only {
        InputAccepted { id, mode, text, images, replaces, held, from }
        InputUnheld { ids }
        EndpointSaved { name, store, check }
        EndpointNotSaved { name, why }
        EndpointEdited { name, changed, store, check }
        EndpointRemoved { name, kept_item, default_cleared }
        Rejected { reason, id, kind }
    }
    by_hand {
        AgentEvent::InputAccepted { opening: true, .. } => return None,
        AgentEvent::AgentSpawned {
            agent,
            session_id,
            config,
            ..
        } => LogEvent::AgentSpawned {
            turn,
            agent: agent.path.clone(),
            session_id: session_id.clone(),
            depth: agent.depth,
            model: agent.model.clone(),
            effort: agent.effort,
            cwd: agent.cwd.clone(),
            config: config.clone(),
            endpoint: agent.endpoint.clone(),
            provider: agent.provider.clone(),
        },
        AgentEvent::AgentStateChanged {
            agent,
            state: wisp_protocol::AgentState::Ended,
            spent_usd,
            plan_usd,
            billing,
            ..
        } => LogEvent::AgentEnded {
            turn,
            agent: agent.clone(),
            spent_usd: *spent_usd,
            plan_usd: plan_usd.or(billing.is_metered().then_some(0.0)),
            billing: *billing,
        },
        AgentEvent::AgentRenamed { agent, to, .. } => LogEvent::AgentRenamed {
            turn,
            agent: agent.clone(),
            to: to.clone(),
        },
        AgentEvent::AgentReported {
            agent,
            turn: reported,
            outcome,
            report,
            duration_ms,
            ..
        } => LogEvent::AgentReported {
            turn: *reported,
            agent: agent.clone(),
            outcome: *outcome,
            report: report.clone(),
            duration_ms: *duration_ms,
        },
        AgentEvent::TodosUpdated {
            call_id: Some(call_id),
            items,
        } => LogEvent::TodosUpdated {
            turn: turn?,
            call_id: call_id.clone(),
            items: items.clone(),
        },
    }
    elsewhere {
        Restart ReadyToRestart
        SubscriptionAuth AccountsGone LeaveNeeds PlanUsageWarning PlanUsageChanged TurnStarted InputDelivered
        AttachmentsWithdrawn Retrying BlockStarted TextDelta ThinkingDelta BlockStopped
        CacheRefreshed ModelAsked SessionNamed CacheDiagnostics AgentStateChanged SpendChanged KeepWarmChanged
        Agent AgentsSnapshot SessionSnapshot LogDisabled SessionResumed TurnEnded StepStarted RequestQueued
        RequestAdmitted StepEnded ToolOutput ToolsChanged SettingsChanged SettingsSnapshot
        FeatureStateChanged HistoryAdapted InstructionsChanged InstructionsWarning ContextChanged
        ContextReport TodosUpdated PastCompactionPoint AutoCompactionHeld ProcessStarted
        ProcessOutput ProcessRestored ProcessDetached AttachRequested AttachRequestClosed
        SecretRequested SecretRequestClosed
        CatalogChanged PluginsChanged SlotContent SlotFailed CommandDone CommandDeferred CompactionStarted
        Compacted CompactionPrepared CompactionFailed RewindPoints ChatsListed SpendListed Rewound Voice
    }
}
