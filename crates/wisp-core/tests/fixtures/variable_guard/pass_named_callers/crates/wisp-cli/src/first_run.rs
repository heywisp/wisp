fn offer(var: &VariableName) -> Option<String> {
    wisp_core::secrets::import::read_var(
        wisp_core::secrets::import::Place::Environment(&crate::real_env),
        var,
    )
}
