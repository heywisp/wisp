mod first_run;
mod manage;

fn home() -> Option<std::ffi::OsString> {
    std::env::var_os("HOME")
}
