fn inside_job() -> bool {
    std::env::var_os(wisp_protocol::WISP_JOB).is_some()
}
