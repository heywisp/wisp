use wisp_render::chrome::KeyHint;
use wisp_render::transcript::copy;

pub const SWITCH_ACCOUNT: (&str, &str) = ("/account", "switch account");
pub const SWITCH_MODEL: (&str, &str) = ("/model", "switch model");

pub fn keys(shift_enter: bool, accounts: usize) -> Vec<KeyHint> {
    let newline = if shift_enter {
        copy::NEW_LINE_KEY_KITTY
    } else {
        copy::NEW_LINE_KEY
    };
    let switch = if accounts > 1 {
        SWITCH_ACCOUNT
    } else {
        SWITCH_MODEL
    };
    vec![
        KeyHint::new(copy::ENTER_KEY, copy::KEY_SEND),
        KeyHint::new(newline, copy::KEY_NEW_LINE),
        KeyHint::note("/help"),
        KeyHint::new(switch.0, switch.1),
    ]
}
