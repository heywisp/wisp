fn all(names: &[&str]) {
    let _ = names.iter().map(std::env::var);
}
