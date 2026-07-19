use std::borrow::Cow;

use ::bstr::{BStr, BString};

use gix_utils::{AsBStr, AsBStrOpt};

#[test]
fn common_string_and_byte_containers_are_supported() {
    let bstring = BString::from("value");
    let string = String::from("value");
    let vec = Vec::from(b"value");
    let array = *b"value";
    let cow = Cow::<BStr>::Borrowed(BStr::new("value"));

    for actual in [
        bstring.as_bstr(),
        string.as_bstr(),
        vec.as_bstr(),
        array.as_bstr(),
        cow.as_bstr(),
        "value".as_bstr(),
    ] {
        assert_eq!(actual, "value", "all supported containers provide the same view");
    }
}

#[test]
fn optional_and_present_values_are_supported_without_allocation() {
    let value = BStr::new("value");
    assert_eq!(value.as_bstr_opt(), Some(value));
    assert_eq!(Some(value).as_bstr_opt(), Some(value));
    assert_eq!(Option::<&BStr>::None.as_bstr_opt(), None);
}
