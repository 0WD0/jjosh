use std::borrow::Cow;

use bstr::{BStr, ByteSlice};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn b(s: &str) -> &bstr::BStr {
    s.into()
}

pub fn cow_str(s: &str) -> Cow<'_, BStr> {
    Cow::Borrowed(s.as_bytes().as_bstr())
}

mod boolean;
mod color;
mod integer;
mod path;

/// Ensure that the `:(optional)` prefix is only recognized for Path types, not for other types
mod optional_prefix_only_for_paths {
    use std::borrow::Cow;
    use gix_config_value::{Boolean, Integer};

    #[test]
    fn optional_prefix_not_recognized_in_boolean() {
        // Boolean should fail to parse this because it's not a valid boolean value
        let result = Boolean::try_from(Cow::Borrowed(crate::b(":(optional)true")));
        assert!(result.is_err(), "Boolean should not recognize :(optional) prefix");
    }

    #[test]
    fn optional_prefix_not_recognized_in_integer() {
        // Integer should fail to parse this because it's not a valid integer value
        let result = Integer::try_from(Cow::Borrowed(crate::b(":(optional)42")));
        assert!(result.is_err(), "Integer should not recognize :(optional) prefix");
    }
}
