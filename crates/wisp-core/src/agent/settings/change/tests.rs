use wisp_protocol::settings::{SettingValue, find, names};

use super::super::kernel::RESTORED_NOW;
use super::{KERNEL_NAMES, checked};

fn auto() -> SettingValue {
    SettingValue::Text("auto".to_owned())
}

#[test]
fn only_a_display_setting_set_to_auto_defers() {
    let theme = find("display.theme").unwrap();
    assert!(theme.defers(Some(&auto())));
    assert!(!theme.defers(Some(&SettingValue::Text("dark".to_owned()))));
    assert!(!theme.defers(None));
    assert!(!find("keychain").unwrap().defers(Some(&auto())));
    assert!(!find("instructions.source").unwrap().defers(Some(&auto())));
}

#[test]
fn only_the_sidebar_lists_take_an_empty_list() {
    let empty = SettingValue::List(Vec::new());
    for name in ["display.sidebar_folded", "display.sidebar_pinned"] {
        let spec = wisp_protocol::settings::find(name).unwrap();
        assert_eq!(checked(spec, &empty), Ok(empty.clone()), "{name}");
    }
    for name in ["tools", "web_search"] {
        let spec = wisp_protocol::settings::find(name).unwrap();
        assert!(checked(spec, &empty).is_err(), "{name} took an empty list");
    }
}

#[test]
fn every_kernel_list_names_a_setting() {
    for name in names::PAIR.iter().chain(&KERNEL_NAMES).chain(&RESTORED_NOW) {
        assert!(find(name).is_some(), "{name}");
    }
    for spec in wisp_protocol::settings::SETTINGS
        .iter()
        .filter(|s| s.settles_on_save())
    {
        assert!(KERNEL_NAMES.contains(&spec.name), "{}", spec.name);
    }
    for name in RESTORED_NOW {
        assert!(KERNEL_NAMES.contains(&name), "{name}");
    }
}

#[test]
fn a_declared_row_is_checked_as_a_kernel_change() {
    use wisp_protocol::plugin::RowKind;
    let spec = crate::plugin::testing::declared(
        "kernel-row",
        RowKind::Int {
            min: 1,
            max: 60,
            default: 10,
        },
    );
    assert_eq!(super::class_of(spec), Ok(super::Class::Kernel));
    assert_eq!(
        checked(spec, &SettingValue::Int(25)),
        Ok(SettingValue::Int(25))
    );
    assert!(checked(spec, &SettingValue::Int(61)).is_err());
}
