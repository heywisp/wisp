use std::sync::OnceLock;

use futures::FutureExt;
use futures::future::BoxFuture;

use wisp_plugin_api::host::{CallContext, Host};
use wisp_plugin_api::pricing::{SpendTotals, Usage as PluginUsage};
use wisp_plugin_api::tools::Prepared as PluginPrepared;

use super::*;
use crate::plugin::testing::{self, Call, TestTool};
use crate::pricing;

#[derive(Default)]
struct Pricer {
    host: OnceLock<Arc<dyn Host>>,
    seen: Mutex<Vec<(SpendTotals, Option<u64>)>>,
}

const MILLION_IN: PluginUsage = PluginUsage {
    input: 1_000_000,
    output: 0,
    cache_read: 0,
    cache_write_5m: 0,
    cache_write_1h: 0,
    web_searches: 0,
};

impl TestTool for Pricer {
    fn name(&self) -> String {
        "pricer".into()
    }

    fn attach(&self, host: Arc<dyn Host>) {
        let _ = self.host.set(host);
    }

    fn prepare(&self, input: &str) -> Result<Box<dyn PluginPrepared>, String> {
        Ok(Call::boxed(testing::fallback("pricer", input), ()))
    }

    fn run<'a>(
        &'a self,
        _call: Box<dyn PluginPrepared>,
        _ctx: &'a dyn CallContext,
    ) -> BoxFuture<'a, wisp_plugin_api::types::ToolRun> {
        async move {
            let host = self.host.get().unwrap();
            let seen = (host.spend(), host.estimate(MILLION_IN));
            self.seen.lock().unwrap().push(seen);
            wisp_plugin_api::types::ToolRun::completed()
        }
        .boxed()
    }
}

fn step_costs(events: &[AgentEvent]) -> Vec<f64> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::StepEnded { cost_usd, .. } => *cost_usd,
            _ => None,
        })
        .collect()
}

fn script() -> Vec<Step> {
    vec![
        uses(&[call("a", "pricer", json!({}))]),
        done("one"),
        uses(&[call("b", "pricer", json!({}))]),
        done("two"),
    ]
}

fn pricer_agent(provider: Arc<Fake>, env: &Env, pricer: &Arc<Pricer>) -> Agent {
    Agent::new(provider, default_session())
        .with_clock(TickingClock::new())
        .with_tools(testing::toolbox(vec![pricer.clone()]), env.workspace())
}

#[tokio::test]
async fn spend_is_the_turn_and_session_cost_so_far_and_estimates_use_the_session_model() {
    let env = Env::new("plugin-pricing");
    let pricer = Arc::new(Pricer::default());
    let mut agent = pricer_agent(scripted(script()), &env, &pricer);
    let (_, first) = turn(&mut agent, "first").await;
    let (_, second) = turn(&mut agent, "second").await;
    let (first, second) = (step_costs(&first), step_costs(&second));
    assert_eq!((first.len(), second.len()), (2, 2));
    let info = pricing::model_info(DEFAULT_MODEL).unwrap();
    let million = pricing::micros(pricing::estimate_usd(
        info,
        &Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        },
        CacheTtl::OneHour,
    ));
    assert_eq!(million, (info.rates.input_per_mtok * 1_000_000.0) as u64);
    let micros = pricing::micros;
    assert_eq!(
        *pricer.seen.lock().unwrap(),
        [
            (
                SpendTotals {
                    turn: Some(micros(first[0])),
                    session: Some(micros(first[0])),
                    ..SpendTotals::default()
                },
                Some(million),
            ),
            (
                SpendTotals {
                    turn: Some(micros(second[0])),
                    session: Some(micros(first[0] + first[1] + second[0])),
                    ..SpendTotals::default()
                },
                Some(million),
            ),
        ]
    );
}

#[tokio::test]
async fn an_unpriced_connection_gives_no_estimate_and_no_spend() {
    let env = Env::new("plugin-pricing-off");
    let pricer = Arc::new(Pricer::default());
    let gateway = Connection {
        name: "gw".into(),
        provider: scripted(script()),
        context: LogContext {
            endpoint: "https://gateway.invalid".into(),
            endpoint_name: "gw".into(),
            ..log_context()
        },
        priced: false,
    };
    let mut agent = pricer_agent(scripted(Vec::new()), &env, &pricer).with_connections(
        vec![gateway],
        Vec::new(),
        "gw",
    );
    turn(&mut agent, "first").await;
    turn(&mut agent, "second").await;
    assert_eq!(
        *pricer.seen.lock().unwrap(),
        [
            (SpendTotals::default(), None),
            (SpendTotals::default(), None)
        ]
    );
}

fn unknown(content: &[B], stop: StopReason) -> Step {
    Step::from(reply_on(UNKNOWN, 600, content, stop))
}

const UNKNOWN: &str = "some-gateway-model";

#[tokio::test]
async fn an_unknown_model_gives_no_estimate_and_no_spend_until_a_step_is_priced() {
    let env = Env::new("plugin-pricing-unknown");
    let pricer = Arc::new(Pricer::default());
    let session = SessionConfig {
        model: UNKNOWN.into(),
        ..default_session()
    };
    let provider = scripted(vec![
        unknown(&[call("a", "pricer", json!({}))], StopReason::ToolUse),
        unknown(&[B::Text("one")], StopReason::EndTurn),
    ]);
    let mut agent = Agent::new(provider, session)
        .with_clock(TickingClock::new())
        .with_tools(testing::toolbox(vec![pricer.clone()]), env.workspace());
    let (_, events) = turn(&mut agent, "first").await;
    assert!(step_costs(&events).is_empty());
    assert_eq!(
        *pricer.seen.lock().unwrap(),
        [(SpendTotals::default(), None)]
    );
}

#[tokio::test]
async fn an_unpriced_step_after_a_priced_one_marks_both_figures_at_least() {
    let env = Env::new("plugin-pricing-at-least");
    let pricer = Arc::new(Pricer::default());
    let provider = scripted(vec![
        uses(&[call("a", "pricer", json!({}))]),
        unknown(&[call("b", "pricer", json!({}))], StopReason::ToolUse),
        done("one"),
    ]);
    let mut agent = pricer_agent(provider, &env, &pricer);
    let (_, events) = turn(&mut agent, "first").await;
    let costs = step_costs(&events);
    assert_eq!(costs.len(), 2);
    let first = Some(pricing::micros(costs[0]));
    let seen: Vec<SpendTotals> = pricer.seen.lock().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(
        seen,
        [
            SpendTotals {
                turn: first,
                turn_at_least: false,
                session: first,
                session_at_least: false,
            },
            SpendTotals {
                turn: first,
                turn_at_least: true,
                session: first,
                session_at_least: true,
            },
        ]
    );
}
