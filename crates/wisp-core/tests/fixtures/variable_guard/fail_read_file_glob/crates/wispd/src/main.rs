use wisp_core::secrets::import::*;

fn load(path: &Path) {
    let _ = read_file(path, false);
}
