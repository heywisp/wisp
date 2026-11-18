use super::*;

async fn chat(case: &'static str, time: MessageTime, note: bool) {
    let setup = Setup {
        time,
        note,
        ..Setup::default()
    };
    let rig = Rig::new(
        case,
        setup,
        vec![says("msg_01", 400, "Hello."), says("msg_02", 520, "Again.")],
    )
    .await;
    let mut live = rig.start();
    live.completed("u1", "hello").await;
    live.completed("u2", "again").await;
    live.close().await;
    rig.check();
}

#[tokio::test]
async fn time_local_with_the_context_note() {
    chat("time_local_note", MessageTime::Local, true).await;
}

#[tokio::test]
async fn time_local_without_the_context_note() {
    chat("time_local_quiet", MessageTime::Local, false).await;
}

#[tokio::test]
async fn time_utc_with_the_context_note() {
    chat("time_utc_note", MessageTime::Utc, true).await;
}

#[tokio::test]
async fn time_utc_without_the_context_note() {
    chat("time_utc_quiet", MessageTime::Utc, false).await;
}

#[tokio::test]
async fn time_off_with_the_context_note() {
    chat("time_off_note", MessageTime::Off, true).await;
}

#[tokio::test]
async fn time_off_without_the_context_note() {
    chat("time_off_quiet", MessageTime::Off, false).await;
}
