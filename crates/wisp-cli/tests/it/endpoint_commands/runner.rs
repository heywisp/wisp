use std::process::{Child, Command, Output, Stdio};

use crate::support::{
    WISP,
    home::{Home, command_in, fed, run},
};

pub(crate) const MODEL: &str = "claude-sonnet-5-5";
pub(crate) const KEY: &str = "sk-ant-CANARYSTOREDKEYa1b2";

impl Home {
    pub(crate) fn endpoint_command(&self, args: &[&str]) -> Command {
        let mut command = command_in(&self.0, WISP);
        command
            .args(["--model", MODEL])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    pub(crate) fn spawn(&self, args: &[&str], stdin: &str) -> Child {
        fed(&mut self.endpoint_command(args), stdin)
    }

    pub(crate) fn endpoint_run(&self, args: &[&str], stdin: &str) -> Output {
        run(&mut self.endpoint_command(args), stdin)
    }

    pub(crate) fn run_env(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
        run(self.endpoint_command(args).envs(env.iter().copied()), stdin)
    }
}
