use std::path::PathBuf;

use wisp_core::session::JsonlStore;

pub fn xdg_config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn xdg_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
}

pub fn models_folder() -> Option<PathBuf> {
    JsonlStore::default_models_dir(xdg_data_home(), home_dir())
}

pub fn used_settings() -> Option<PathBuf> {
    JsonlStore::default_dir(xdg_data_home(), home_dir())
        .map(|dir| dir.with_file_name("settings-used.json"))
}

pub fn usage_cache() -> wisp_core::plan_usage::PlanUsageCache {
    JsonlStore::default_usage_dir(xdg_data_home(), home_dir()).map_or_else(
        wisp_core::plan_usage::PlanUsageCache::default,
        wisp_core::plan_usage::PlanUsageCache::shared,
    )
}

pub fn remembered() -> wisp_core::settings::remembered::RememberedStore {
    JsonlStore::default_dir(xdg_data_home(), home_dir()).map_or_else(
        wisp_core::settings::remembered::RememberedStore::default,
        |dir| {
            wisp_core::settings::remembered::RememberedStore::shared(
                dir.with_file_name("remembered"),
            )
        },
    )
}

#[cfg(any(test, feature = "test-support"))]
static PROCESS_ENV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn use_process_env() {
    #[cfg(any(test, feature = "test-support"))]
    PROCESS_ENV.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn process_env_used() -> bool {
    PROCESS_ENV.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn real_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn real_vars() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}
