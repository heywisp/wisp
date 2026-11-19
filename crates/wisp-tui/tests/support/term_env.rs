use std::process::Command;

use wisp_tui::term::caps::ENV_KEYS;
pub(crate) const ISOLATED_WSL_ROOT: &str = "/dev/null";

pub(crate) fn clean_term_env(command: &mut Command) -> &mut Command {
    for key in ENV_KEYS {
        command.env_remove(key);
    }
    command
        .env("WISP_WSL_ROOT", ISOLATED_WSL_ROOT)
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
}
