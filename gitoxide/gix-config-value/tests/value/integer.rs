use gix_config_value::{Integer, integer::Suffix};

#[test]
fn from_utf8_str() -> crate::Result {
    assert_eq!(
        Integer::try_from("1k")?,
        Integer {
            value: 1,
            suffix: Some(Suffix::Kibi),
        },
        "UTF-8 strings use the same integer parser as byte strings"
    );
    Ok(())
}

#[test]
fn from_str_no_suffix() {
    assert_eq!(Integer::try_from("1").unwrap(), Integer { value: 1, suffix: None });

    assert_eq!(
        Integer::try_from("-1").unwrap(),
        Integer {
            value: -1,
            suffix: None
        }
    );
}

#[test]
fn from_str_with_suffix() {
    assert_eq!(
        Integer::try_from("1k").unwrap(),
        Integer {
            value: 1,
            suffix: Some(Suffix::Kibi),
        }
    );

    assert_eq!(
        Integer::try_from("1m").unwrap(),
        Integer {
            value: 1,
            suffix: Some(Suffix::Mebi),
        }
    );

    assert_eq!(
        Integer::try_from("1g").unwrap(),
        Integer {
            value: 1,
            suffix: Some(Suffix::Gibi),
        }
    );
}

#[test]
fn invalid_from_str() {
    assert!(Integer::try_from("").is_err());
    assert!(Integer::try_from("-").is_err());
    assert!(Integer::try_from("k").is_err());
    assert!(Integer::try_from("m").is_err());
    assert!(Integer::try_from("g").is_err());
    assert!(Integer::try_from("123123123123123123123123").is_err());
    assert!(Integer::try_from("gg").is_err());
    assert!(Integer::try_from("™️🤦‍♂️").is_err());
}

#[test]
fn as_decimal() {
    fn decimal(input: &str) -> Option<i64> {
        Integer::try_from(input).unwrap().to_decimal()
    }

    assert_eq!(decimal("12"), Some(12), "works without suffix");
    assert_eq!(decimal("13k"), Some(13 * 1024), "works with kilobyte suffix");
    assert_eq!(decimal("13K"), Some(13 * 1024), "works with Kilobyte suffix");
    assert_eq!(decimal("14m"), Some(14 * 1_048_576), "works with megabyte suffix");
    assert_eq!(decimal("14M"), Some(14 * 1_048_576), "works with Megabyte suffix");
    assert_eq!(decimal("15g"), Some(15 * 1_073_741_824), "works with gigabyte suffix");
    assert_eq!(decimal("15G"), Some(15 * 1_073_741_824), "works with Gigabyte suffix");

    assert_eq!(decimal(&format!("{}g", i64::MAX)), None, "overflow results in None");
    assert_eq!(decimal(&format!("{}g", i64::MIN)), None, "underflow results in None");
}
