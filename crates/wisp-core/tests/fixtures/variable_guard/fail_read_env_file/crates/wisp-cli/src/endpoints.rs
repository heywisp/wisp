fn read(path: &Path) {
    let _ = wisp_core::secrets::read_env_file(path, false);
}
