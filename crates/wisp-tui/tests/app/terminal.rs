use std::future::Future;

use tokio::sync::mpsc;
use wisp_protocol::{ClientCommand, TermSize};
use wisp_tui::term::{Input, KeyCode};

use super::Screen;
use super::sidebar::{ROWS, both, drain, no_rule, options_in, quit, rule_at};
use super::steering::{Agent, steering_through};
use crate::Drive;
use wisp_render::theme::Mode;

const WIDE: u16 = 160;

fn terminal(cols: u16) -> ClientCommand {
    ClientCommand::Terminal {
        size: TermSize { cols, rows: ROWS },
    }
}

fn sizes(commands: &[ClientCommand]) -> Vec<ClientCommand> {
    commands
        .iter()
        .filter(|c| matches!(c, ClientCommand::Terminal { .. }))
        .cloned()
        .collect()
}

async fn raw<D, F>(capacity: usize, drive: D)
where
    D: FnOnce(Agent) -> F,
    F: Future<Output = ()>,
{
    let screen = Screen::new(ROWS, WIDE);
    let channel = mpsc::channel(capacity);
    steering_through(
        screen,
        options_in(Mode::Dark),
        super::facts(),
        channel,
        drive,
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_reply_width_is_sent_first_at_start_then_once_per_change_with_the_sidebar() {
    raw(64, |mut a| async move {
        let start = drain(&mut a).await;
        assert_eq!(sizes(&start), [terminal(156)], "{start:?}");
        a.emit(both()).await;
        a.settle().await;
        assert_eq!(sizes(&drain(&mut a).await), []);
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        assert!(rule_at(&a.screen, 119));
        assert_eq!(sizes(&drain(&mut a).await), [terminal(115)]);
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        assert!(no_rule(&a.screen));
        assert_eq!(sizes(&drain(&mut a).await), [terminal(156)]);
        a.paste("hello").key(KeyCode::Enter);
        let sent = drain(&mut a).await;
        assert!(
            sent.iter().any(|c| matches!(c, ClientCommand::Send { .. })),
            "{sent:?}"
        );
        assert_eq!(sizes(&sent), []);
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_size_that_could_not_be_sent_is_sent_at_the_next_check() {
    raw(1, |mut a| async move {
        a.emit(both()).await;
        a.paste("/sidebar").key(KeyCode::Enter);
        a.settle().await;
        assert!(rule_at(&a.screen, 119));
        assert_eq!(a.next().await, terminal(156));
        a.nothing_sent().await;
        a.paste("hello").key(KeyCode::Enter);
        assert_eq!(a.next().await, terminal(115));
        a.nothing_sent().await;
        a.key(KeyCode::Enter);
        assert!(
            matches!(a.next().await, ClientCommand::Send { .. }),
            "the message follows its size"
        );
        quit(&a).await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_message_during_a_resize_waits_for_the_settled_size() {
    raw(64, |mut a| async move {
        assert_eq!(sizes(&drain(&mut a).await), [terminal(156)]);
        let size = a.screen.resize(40, 140);
        a.inputs.send(Input::Resize(size)).unwrap();
        a.paste("hello").key(KeyCode::Enter);
        let sent = drain(&mut a).await;
        assert!(
            sent.iter().any(|c| matches!(c, ClientCommand::Send { .. })),
            "{sent:?}"
        );
        assert_eq!(sizes(&sent), []);
        a.settle().await;
        let settled = ClientCommand::Terminal {
            size: TermSize {
                cols: 136,
                rows: 40,
            },
        };
        assert_eq!(sizes(&drain(&mut a).await), [settled]);
        quit(&a).await;
    })
    .await;
}
