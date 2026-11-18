use std::env::var as get;

fn read(name: &str) {
    let _ = get(name);
}
