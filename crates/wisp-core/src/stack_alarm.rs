use std::future::Future;
use std::panic::resume_unwind;
use std::pin::pin;
use std::thread;

pub(crate) const STACK_ALARM_BYTES: usize = 1536 * 1024;

pub(crate) enum Timer {
    Paused,
    Real,
}

pub(crate) fn on_alarm_stack<F: Future<Output = ()>>(
    timer: Timer,
    test: impl FnOnce() -> F + Send,
) {
    let name = thread::current().name().unwrap_or("test").to_owned();
    thread::scope(|scope| {
        let run = thread::Builder::new()
            .name(name)
            .stack_size(STACK_ALARM_BYTES)
            .spawn_scoped(scope, move || {
                let future = pin!(test());
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .start_paused(matches!(timer, Timer::Paused))
                    .build()
                    .unwrap()
                    .block_on(future);
            })
            .unwrap();
        if let Err(panic) = run.join() {
            resume_unwind(panic);
        }
    });
}
