use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use wasmtime::{Cache, CacheConfig, Config, Engine};

const TICK: Duration = Duration::from_millis(10);

static ENGINE: OnceLock<Result<Engine, String>> = OnceLock::new();

pub(crate) fn engine(cache: Option<&Path>) -> Result<&'static Engine, String> {
    ENGINE
        .get_or_init(|| start(cache))
        .as_ref()
        .map_err(Clone::clone)
}

fn start(cache: Option<&Path>) -> Result<Engine, String> {
    let mut config = Config::new();
    config
        .wasm_component_model(true)
        .wasm_component_model_async(true)
        .concurrency_support(true)
        .epoch_interruption(true)
        .cache(cache.and_then(compiled));
    let engine = Engine::new(&config).map_err(|e| e.to_string())?;
    let ticking = engine.clone();
    std::thread::Builder::new()
        .name("wisp-wasm-epoch".to_owned())
        .spawn(move || {
            loop {
                std::thread::sleep(TICK);
                ticking.increment_epoch();
            }
        })
        .map_err(|e| format!("the WASM clock thread didn't start: {e}"))?;
    Ok(engine)
}

fn compiled(dir: &Path) -> Option<Cache> {
    let mut config = CacheConfig::new();
    config.with_directory(dir);
    Cache::new(config).ok()
}
