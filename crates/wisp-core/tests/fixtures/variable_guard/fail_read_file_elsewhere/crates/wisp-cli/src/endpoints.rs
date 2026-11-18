fn read(path: &Path) {
    let _ = wisp_core::secrets::import::read_file(path, false);
}
