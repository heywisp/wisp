use std::fmt;

use crate::provider::credential::EnvFile;

pub type Lookup<'a> = &'a (dyn Fn(&str) -> Option<String> + Sync);

pub struct Variables<'a> {
    env: Lookup<'a>,
    env_file: Option<&'a EnvFile>,
    explicit_env_file: bool,
}

impl<'a> Variables<'a> {
    pub fn new(env: Lookup<'a>, env_file: Option<&'a EnvFile>, explicit_env_file: bool) -> Self {
        Self {
            env,
            env_file,
            explicit_env_file,
        }
    }

    pub fn env(&self) -> Lookup<'a> {
        self.env
    }

    pub fn env_file(&self) -> Option<&'a EnvFile> {
        self.env_file
    }

    pub fn explicit_env_file(&self) -> bool {
        self.explicit_env_file
    }
}

impl fmt::Debug for Variables<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Variables")
            .field("env_file", &self.env_file)
            .field("explicit_env_file", &self.explicit_env_file)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum Keychain<'a> {
    Available(&'a str),
    Absent(Variables<'a>),
}

impl<'a> Keychain<'a> {
    pub fn variables(&self) -> Option<&Variables<'a>> {
        match self {
            Self::Available(_) => None,
            Self::Absent(variables) => Some(variables),
        }
    }
}
