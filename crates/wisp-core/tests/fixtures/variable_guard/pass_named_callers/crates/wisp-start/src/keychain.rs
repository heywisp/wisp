impl KeychainInputs {
    fn load(path: &Path) -> Self {
        let _ = wisp_core::secrets::read_env_file(path, false);
        Self
    }

    fn redactions(&self) -> Redactions {
        wisp_core::secrets::variable_redactions(None, &crate::real_env, None)
    }
}

fn keychain(inputs: &KeychainInputs) -> Keychain<'_> {
    let env: Lookup = &crate::real_env;
    Keychain::Absent(Variables::new(env, None, false))
}
