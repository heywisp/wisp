fn load(path: &Path) {
    let _ = crate::secrets::import::read_file(path, false);
}
