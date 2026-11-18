fn other() -> bool {
    std::env::var_os(wisp_protocol::OTHER).is_some()
}
