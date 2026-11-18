fn display_env() -> Option<String> {
    crate::paths::real_env("NO_COLOR")
}
