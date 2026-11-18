use std::process::Command;

use wisp_tui::term::caps::ENV_KEYS;

pub(crate) fn clean_term_env(command: &mut Command) -> &mut Command {
    for key in ENV_KEYS {
        command.env_remove(key);
    }
    command.env("TERM", "xterm-256color").env("LANG", "C.UTF-8")
}
