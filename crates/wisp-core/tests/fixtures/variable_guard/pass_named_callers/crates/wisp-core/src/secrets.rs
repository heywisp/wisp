fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v.to_str().is_some_and(|s| !s.trim().is_empty()))
}
