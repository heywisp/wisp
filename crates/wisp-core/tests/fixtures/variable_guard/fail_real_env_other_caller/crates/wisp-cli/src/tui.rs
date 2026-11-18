fn read(name: &VariableName) {
    let _ = wisp_core::secrets::import::read_var(
        wisp_core::secrets::import::Place::Environment(&crate::real_env),
        name,
    );
}
