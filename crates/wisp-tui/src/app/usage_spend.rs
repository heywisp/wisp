use wisp_protocol::{AccountSpend, SpendDay, SpendWindow};
use wisp_render::chrome::session_total;
use wisp_render::plan_usage::{AllChats, Spend};

use super::agents::Agents;

pub(super) fn endpoint_view(agents: &Agents, endpoint: &str, priced: bool) -> Spend {
    let mut amount = AccountSpend::default();
    for accounts in agents.spends() {
        if let Some(spent) = accounts.get(endpoint) {
            amount.add(*spent);
        }
    }
    Spend { amount, priced }
}

pub(super) fn page_total(agents: &Agents) -> Spend {
    Spend {
        amount: session_total(agents.main_spend(), agents.rows()),
        priced: true,
    }
}

pub(super) fn all_chats<'a>(
    accounts: impl IntoIterator<Item = &'a str>,
    days: &[SpendDay],
    window: &SpendWindow,
) -> Option<AllChats> {
    let today = days.first()?;
    let spend = |amount| Spend {
        amount,
        priced: true,
    };
    Some(AllChats {
        accounts: accounts
            .into_iter()
            .map(|account| {
                let amounts = [
                    (&today.accounts, today.total),
                    (&window.accounts, window.total),
                ]
                .map(|(map, total)| {
                    spend(map.get(account).copied().unwrap_or(AccountSpend {
                        lower_bound: total.lower_bound
                            && map.values().all(|amount| amount.lower_bound),
                        ..AccountSpend::default()
                    }))
                });
                (account.to_owned(), amounts)
            })
            .collect(),
        today: spend(today.total),
        window: spend(window.total),
        label: super::reset_time::spend_window(window.since.as_deref()),
        days: days
            .iter()
            .enumerate()
            .map(|(index, day)| {
                (
                    super::reset_time::spend_day(index, &day.day),
                    spend(day.total),
                )
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{AgentEntry, AgentEvent, Billing};

    use super::*;

    fn spent(usd: f64, plan_usd: Option<f64>, lower_bound: bool) -> AccountSpend {
        AccountSpend {
            usd,
            plan_usd,
            lower_bound,
        }
    }

    fn table(main: &[(&str, AccountSpend)], children: Vec<AgentEntry>) -> Agents {
        let accounts = main.iter().map(|(name, s)| ((*name).to_owned(), *s));
        let accounts: std::collections::BTreeMap<_, _> = accounts.collect();
        let mut total = AccountSpend::default();
        accounts.values().for_each(|s| total.add(*s));
        let facts = crate::app::fixture::endpoint_facts();
        let mut agents = Agents::default();
        agents.apply(&AgentEvent::SpendChanged { total, accounts }, 0, &facts);
        let snapshot = AgentEvent::AgentsSnapshot {
            request: None,
            agents: children,
        };
        agents.apply(&snapshot, 0, &facts);
        agents
    }

    fn child(
        path: &str,
        usd: f64,
        billing: Billing,
        accounts: &[(&str, AccountSpend)],
    ) -> AgentEntry {
        let mut entry: AgentEntry = serde_json::from_value(serde_json::json!({
            "path": path, "name": "c", "model": "m", "effort": "low", "depth": 1,
            "state": {"state": "running"}, "activity": {"type": "idle"}, "spent_usd": usd,
        }))
        .unwrap();
        entry.billing = billing;
        entry.accounts = accounts
            .iter()
            .map(|(n, s)| ((*n).to_owned(), *s))
            .collect();
        entry
    }

    #[test]
    fn each_account_shows_only_the_spend_it_served_and_unnamed_spend_counts_in_the_total() {
        let older = child("/older", 0.5, Billing::Metered, &[]);
        let agents = table(
            &[
                ("claude1", spent(1.0, Some(1.0), false)),
                ("env", spent(0.25, Some(0.0), false)),
            ],
            vec![older],
        );
        let chat = endpoint_view(&agents, "claude-main", true);
        assert_eq!(
            (
                chat.amount.usd,
                chat.amount.plan_usd,
                chat.amount.lower_bound
            ),
            (0.0, Some(0.0), false)
        );
        let claude1 = endpoint_view(&agents, "claude1", true);
        assert_eq!(
            (claude1.amount.usd, claude1.amount.plan_usd),
            (1.0, Some(1.0))
        );
        let env = endpoint_view(&agents, "env", true);
        assert_eq!((env.amount.usd, env.amount.plan_usd), (0.25, Some(0.0)));
        let total = page_total(&agents);
        assert_eq!((total.amount.usd, total.amount.plan_usd), (1.75, Some(1.0)));
    }

    #[test]
    fn a_childs_spend_counts_in_each_account_of_its_split_and_an_unknown_split_stays_unsplit() {
        let split = [
            ("a", spent(0.25, Some(0.25), false)),
            ("b", spent(0.5, None, false)),
        ];
        let worker = child("/worker", 1.0, Billing::Mixed, &split);
        let agents = table(&[("a", spent(0.5, Some(0.5), false))], vec![worker]);
        let a = endpoint_view(&agents, "a", true);
        assert_eq!((a.amount.usd, a.amount.plan_usd), (0.75, Some(0.75)));
        let b = endpoint_view(&agents, "b", true);
        assert_eq!((b.amount.usd, b.amount.plan_usd), (0.5, None));
        let total = page_total(&agents);
        assert_eq!((total.amount.usd, total.amount.plan_usd), (1.5, None));
        assert_eq!(endpoint_view(&agents, "c", false).amount.usd, 0.0);
    }

    #[test]
    fn a_zero_dollar_lower_bound_still_marks_the_endpoint_estimate() {
        let unpriced = child(
            "/free",
            0.0,
            Billing::Metered,
            &[("api", spent(0.0, Some(0.0), true))],
        );
        let agents = table(&[], vec![unpriced]);
        assert!(endpoint_view(&agents, "api", true).amount.lower_bound);
        assert!(!endpoint_view(&agents, "other", true).amount.lower_bound);
    }

    #[test]
    fn all_chat_lookups_keep_the_kernel_totals_and_unlisted_accounts_out_of_blocks() {
        let day = SpendDay {
            day: "2026-11-14".into(),
            accounts: [
                ("listed".into(), spent(2.0, Some(0.0), false)),
                ("gone".into(), spent(7.0, Some(7.0), true)),
            ]
            .into(),
            total: spent(9.0, Some(7.0), true),
        };
        let window = SpendWindow {
            since: Some("2026-11-12".into()),
            accounts: [("listed".into(), spent(8.0, None, true))].into(),
            total: spent(500.0, None, true),
        };
        let days = [
            day.clone(),
            SpendDay {
                day: "2026-11-13".into(),
                ..day.clone()
            },
            SpendDay {
                day: "2026-11-12".into(),
                ..day
            },
        ];
        let all = all_chats(["listed", "zero"], &days, &window).unwrap();
        assert_eq!(all.accounts.len(), 2);
        assert_eq!(all.accounts["listed"][0].amount, days[0].accounts["listed"]);
        assert_eq!(all.accounts["listed"][1].amount, window.accounts["listed"]);
        assert_eq!(
            all.accounts["zero"].map(|spend| spend.amount),
            [spent(0.0, Some(0.0), false), spent(0.0, Some(0.0), true)]
        );
        for lower_bound in [false, true] {
            let total = spent(0.0, Some(0.0), lower_bound);
            let empty = all_chats(
                ["zero"],
                &[SpendDay {
                    total,
                    ..SpendDay::default()
                }],
                &SpendWindow {
                    total,
                    ..SpendWindow::default()
                },
            )
            .unwrap();
            assert_eq!(empty.accounts["zero"].map(|spend| spend.amount), [total; 2]);
        }
        assert_eq!(all.today.amount, days[0].total);
        assert_eq!(all.window.amount, window.total);
        assert_eq!(all.label, "since 12 Nov");
        assert_eq!(
            all.days
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            ["Today", "Yesterday", "Thu"]
        );
        assert_eq!(super::super::reset_time::spend_window(None), "30 days");
        assert!(all_chats(["listed"], &[], &window).is_none());
    }
}
