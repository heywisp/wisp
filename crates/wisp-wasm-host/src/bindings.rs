#![allow(
    unsafe_code,
    reason = "wasmtime's bindgen! derives unsafe ComponentType impls"
)]
#![allow(
    unreachable_pub,
    clippy::all,
    reason = "the code bindgen! generates is not ours to lint"
)]

wasmtime::component::bindgen!({
    inline: "package wisp:wasm-host; world served { include wisp:plugin/plugin@0.2.0; include wisp:plugin/plugin@0.3.0; }",
    path: ["../../wit/v0.1.0", "../../wit/v0.2.0", "../../wit/v0.3.0"],
    imports: {
        "wisp:plugin/call": async | trappable,
        "wisp:plugin/jobs.adopt": async | trappable,
        "wisp:plugin/jobs.mark-reported": async | trappable,
        "wisp:plugin/todos": async | trappable,
        "wisp:plugin/agents": async | trappable,
        "wisp:plugin/agent-rename": async | trappable,
        "wisp:plugin/config": async | trappable,
        default: trappable,
    },
    with: {
        "wisp:plugin/call.call-context": crate::context::CallLink,
        "wisp:plugin/jobs.job": crate::jobs::JobHandle,
        "wisp:plugin/http.body": crate::imports::http::BodyHandle,
        "wisp:plugin/history.view": crate::imports::Viewed,
    },
});
