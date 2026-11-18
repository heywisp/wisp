fn read(path: &Path, lookup: Lookup<'_>) {
    let _ = super::read_env_file(path, false);
    let _ = Variables::new(lookup, None, false);
}
