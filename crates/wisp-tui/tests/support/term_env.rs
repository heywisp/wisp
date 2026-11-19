use std::process::Command;

use wisp_tui::term::caps::ENV_KEYS;
use wisp_tui::term::host::WSL_ROOT_ENV;

pub(crate) fn clean_term_env(command: &mut Command) -> &mut Command {
    for key in ENV_KEYS {
        command.env_remove(key);
    }
    command
        .env(WSL_ROOT_ENV, "/dev/null")
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
}
