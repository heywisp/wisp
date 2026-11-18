fn is_set(name: &str) -> bool {
    std::env::var_os(name).map(|v| !v.is_empty()).unwrap_or(false)
}
