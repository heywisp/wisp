use wisp_core::secrets::import as imp;

fn load(path: &Path) {
    let _ = imp::read_file(path, false);
}
