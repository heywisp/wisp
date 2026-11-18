fn read(path: &Path) {
    let _ = secrets::import::read_file(path, false);
}
