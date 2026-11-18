fn pick(keychain: &Keychain<'_>) -> bool {
    match keychain {
        Keychain::Absent(variables) => variables.explicit_env_file(),
        Keychain::Available => false,
    }
}
