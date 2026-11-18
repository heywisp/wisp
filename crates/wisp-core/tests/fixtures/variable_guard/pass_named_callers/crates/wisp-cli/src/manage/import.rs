fn has(var: &VariableName) -> bool {
    wisp_core::secrets::import::read_var(
        wisp_core::secrets::import::Place::Environment(&crate::real_env),
        var,
    )
    .is_some()
}
