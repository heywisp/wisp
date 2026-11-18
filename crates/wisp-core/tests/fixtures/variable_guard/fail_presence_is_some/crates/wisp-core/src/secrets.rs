fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}
