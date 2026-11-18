fn is_set(name: &str) -> bool {
    std::env::var(name).is_ok()
}
