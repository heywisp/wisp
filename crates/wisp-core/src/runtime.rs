use std::io;
use std::num::NonZero;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use tokio::runtime::{Builder, Runtime};

pub const THREAD_STACK_BYTES: usize = 16 * 1024 * 1024;

pub fn build(builder: &mut Builder) -> io::Result<Runtime> {
    builder
        .enable_all()
        .thread_stack_size(THREAD_STACK_BYTES)
        .build()
}

pub(crate) fn in_parallel<T: Sync, R: Send + Sync>(
    items: &[T],
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let done: Vec<OnceLock<R>> = items.iter().map(|_| OnceLock::new()).collect();
    let next = AtomicUsize::new(0);
    let worker = || {
        loop {
            let at = next.fetch_add(1, Ordering::Relaxed);
            let (Some(item), Some(slot)) = (items.get(at), done.get(at)) else {
                break;
            };
            slot.set(work(item)).ok();
        }
    };
    let cores = thread::available_parallelism().map_or(1, NonZero::get);
    thread::scope(|scope| {
        for _ in 1..cores.min(items.len()) {
            thread::Builder::new()
                .stack_size(THREAD_STACK_BYTES)
                .spawn_scoped(scope, worker)
                .ok();
        }
        worker();
    });
    done.into_iter().filter_map(OnceLock::into_inner).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thread_stack_matches_the_test_stack_in_cargo_config() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.cargo/config.toml");
        let text = std::fs::read_to_string(path).unwrap();
        let config: toml::Table = toml::from_str(&text).unwrap();
        let test_stack = config["env"]["RUST_MIN_STACK"].as_str().unwrap();
        assert_eq!(test_stack, THREAD_STACK_BYTES.to_string());
    }
}
