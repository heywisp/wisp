mod edit;
mod import;

fn place() -> Place<'static> {
    wisp_core::secrets::import::Place::Environment(&crate::real_env)
}
