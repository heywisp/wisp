use wisp_tool_text::jobs::typed_line;

use super::*;

async fn ended(rig: &mut Rig, scope: &Scope, script: &str) -> u32 {
    let job = rig.spawn(scope, rig.sh(script)).await;
    assert!(matches!(waited(&job, for_end()).await, Waited::Ended(_)));
    let _ = rig.jobs.processes().notes(true);
    job.info().await.id.unwrap()
}

fn call(id: &str) -> Carrier {
    Carrier::Call(id.to_owned())
}

#[tokio::test]
async fn an_end_is_carried_by_the_call_that_takes_it_and_heard_by_that_call_only() {
    let mut rig = Rig::new();
    let first = Scope::new("c1");
    let id = ended(&mut rig, &first, "exit 0").await;
    let other = Scope::new("c2");
    assert_eq!(
        rig.jobs.take_ends(&other),
        [format!("Process {id} (sh) exited with code 0.")]
    );
    assert!(rig.jobs.take_ends(&first).is_empty());
    assert!(rig.jobs.processes().hear(&call("c1")).ids.is_empty());
    assert_eq!(rig.jobs.processes().hear(&call("c2")).ids, [id]);
    assert!(rig.jobs.take_ends(&Scope::new("c3")).is_empty());
}

#[tokio::test]
async fn a_result_that_showed_an_end_marks_it_reported_for_its_own_call() {
    let mut rig = Rig::new();
    let first = Scope::new("c1");
    let id = ended(&mut rig, &first, "exit 4").await;
    rig.jobs.mark_reported(id, &first);
    assert!(rig.jobs.take_ends(&Scope::new("c2")).is_empty());
    assert_eq!(rig.jobs.processes().hear(&call("c1")).ids, [id]);
}

#[tokio::test]
async fn an_unheard_end_is_offered_again_to_a_later_call() {
    let mut rig = Rig::new();
    let first = Scope::new("c1");
    let id = ended(&mut rig, &first, "exit 0").await;
    assert_eq!(rig.jobs.take_ends(&first).len(), 1);
    rig.jobs.processes().unoffer(&call("c1"));
    let later = Scope::new("c2");
    assert_eq!(
        rig.jobs.take_ends(&later),
        [format!("Process {id} (sh) exited with code 0.")]
    );
}

#[test]
fn a_typed_detach_owes_one_line_that_is_carried_once() {
    let rig = Rig::new();
    let processes = rig.jobs.processes();
    processes.typed(3);
    assert_eq!(rig.jobs.take_ends(&Scope::new("c1")), [typed_line(3)]);
    assert!(rig.jobs.take_ends(&Scope::new("c2")).is_empty());
    processes.unoffer(&call("c1"));
    assert_eq!(rig.jobs.take_ends(&Scope::new("c2")), [typed_line(3)]);
    let heard = processes.hear(&call("c2"));
    assert_eq!(heard.typed, [3]);
    assert!(heard.ids.is_empty());
    assert!(rig.jobs.take_ends(&Scope::new("c3")).is_empty());
}

#[tokio::test]
async fn a_typed_line_comes_before_the_ends() {
    let mut rig = Rig::new();
    let first = Scope::new("c1");
    let id = ended(&mut rig, &first, "exit 0").await;
    rig.jobs.processes().typed(id);
    assert_eq!(
        rig.jobs.take_ends(&first),
        [
            typed_line(id),
            format!("Process {id} (sh) exited with code 0.")
        ]
    );
}
