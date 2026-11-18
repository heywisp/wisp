fn real_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
