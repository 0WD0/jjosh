use std::borrow::Cow;

use bstr::{BStr, BString};
use gix_config_value::{Integer, integer::Suffix};
use gix_error::MetadataValue;

#[test]
fn from_utf8_str() -> gix_testtools::TestResult {
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
fn from_bytes_accepts_common_inputs() -> gix_testtools::TestResult {
    let owned = BString::from("0X800");
    for actual in [
        Integer::from_bytes::<i64>("2k")?,
        Integer::from_bytes(String::from("2048"))?,
        Integer::from_bytes(b"0x800")?,
        Integer::from_bytes(b"04000".as_slice())?,
        Integer::from_bytes(b"+2048".to_vec())?,
        Integer::from_bytes(BStr::new("0b10k"))?,
        Integer::from_bytes(&owned)?,
        Integer::from_bytes(owned)?,
        Integer::from_bytes(Cow::Borrowed(BStr::new("02K")))?,
        Integer::from_bytes(Cow::<'_, BStr>::Owned(BString::from("+0B10K")))?,
    ] {
        assert_eq!(actual, 2048, "all input types accept equivalent integer spellings");
    }
    let signed: i64 = Integer::from_bytes("-2k")?;
    let unsigned: usize = Integer::from_bytes("2k")?;
    assert_eq!(signed, -2048, "the signed target can be inferred");
    assert_eq!(unsigned, 2048, "the unsigned target can be inferred");
    Ok(())
}

#[test]
fn from_bytes_applies_suffixes_before_converting_to_the_target() -> gix_testtools::TestResult {
    for (input, expected) in [
        ("12", 12),
        ("13k", 13 * 1024),
        ("13K", 13 * 1024),
        ("14m", 14 * 1_048_576),
        ("14M", 14 * 1_048_576),
        ("15g", 15 * 1_073_741_824),
        ("15G", 15 * 1_073_741_824),
        ("9223372036854775807", i64::MAX),
        ("-9223372036854775808", i64::MIN),
        ("-8589934592g", i64::MIN),
    ] {
        assert_eq!(
            Integer::from_bytes::<i64>(input)?,
            expected,
            "{input:?}: signed values include the suffix multiplier"
        );
    }
    assert_eq!(
        Integer::from_bytes::<u8>("255")?,
        u8::MAX,
        "unsigned bounds are inclusive"
    );
    assert_eq!(
        Integer::from_bytes::<u16>("63k")?,
        64_512,
        "the multiplied value fits in u16"
    );
    assert_eq!(
        Integer::from_bytes::<i16>("-32k")?,
        i16::MIN,
        "signed bounds are inclusive"
    );
    assert_eq!(Integer::from_bytes::<u64>("-0k")?, 0, "negative zero remains zero");
    assert_eq!(
        Integer::from_bytes::<u64>("9223372036854775807")?,
        i64::MAX as u64,
        "unsigned targets retain the existing signed parser range"
    );
    Ok(())
}

#[test]
fn from_bytes_classifies_parse_and_suffix_errors() {
    for input in [
        b"".as_slice(),
        b"k",
        b"08",
        b"9223372036854775808",
        b"-9223372036854775809",
        b"8589934592g",
        b"-8589934593g",
        b"\xff",
    ] {
        let err = Integer::from_bytes::<i64>(input).expect_err("invalid or overflowing integers are rejected");
        assert_validation_input(&err, input);
    }
    let err = Integer::from_bytes::<u64>("9223372036854775808")
        .expect_err("an unsigned target does not widen the signed parser range");
    assert_validation_input(&err, b"9223372036854775808");

    let err = Integer::from_bytes::<i64>(b"\xff").expect_err("integer text must be UTF-8");
    assert!(
        err.downcast_any_ref::<std::str::Utf8Error>().is_some(),
        "the original decoding error remains available"
    );
}

#[test]
fn from_bytes_classifies_target_range_errors() {
    for (input, result) in [
        ("-1", Integer::from_bytes::<u8>("-1").map(i64::from)),
        ("256", Integer::from_bytes::<u8>("256").map(i64::from)),
        ("128", Integer::from_bytes::<i8>("128").map(i64::from)),
        ("-129", Integer::from_bytes::<i8>("-129").map(i64::from)),
        ("64k", Integer::from_bytes::<u16>("64k").map(i64::from)),
        ("4294967296", Integer::from_bytes::<u32>("4294967296").map(i64::from)),
    ] {
        let err = result.expect_err("the multiplied integer must fit in the target type");
        assert_validation_input(&err, input.as_bytes());
        assert!(
            err.downcast_any_ref::<std::num::TryFromIntError>().is_some(),
            "the original target conversion error remains available"
        );
    }
}

fn assert_validation_input(err: &gix_error::Error, input: &[u8]) {
    assert!(err.is_validation(), "invalid integers are validation errors: {err}");
    assert_eq!(
        err.metadata().find_map(|metadata| metadata.get("input")),
        Some(&MetadataValue::Bytes(input.into())),
        "errors retain the original input bytes, including invalid UTF-8"
    );
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

/// git hands config integers to `strtoimax()` with a base of `0`, so a `0x` prefix is
/// hexadecimal, a `0b` prefix is binary, and a leading `0` is octal.
#[test]
fn bases_match_git() {
    fn decimal(input: &str) -> Option<i64> {
        Integer::from_bytes(input).ok()
    }

    assert_eq!(decimal("0x10"), Some(16), "0x is hexadecimal");
    assert_eq!(decimal("0X1F"), Some(31), "the prefix is case insensitive");
    assert_eq!(decimal("+0x10"), Some(16), "a positive sign precedes the prefix");
    assert_eq!(decimal("-0x10"), Some(-16), "a sign precedes the prefix");
    assert_eq!(decimal("0b101"), Some(5), "0b is binary");
    assert_eq!(decimal("0B101"), Some(5), "the prefix is case insensitive");
    assert_eq!(decimal("+0b101"), Some(5), "binary values may have a positive sign");
    assert_eq!(decimal("-0b101"), Some(-5), "binary values may have a negative sign");
    assert_eq!(decimal("010"), Some(8), "a leading zero is octal, not decimal");
    assert_eq!(decimal("+010"), Some(8), "octal values may have a positive sign");
    assert_eq!(decimal("-010"), Some(-8), "octal values may have a negative sign");
    assert_eq!(decimal("00"), Some(0), "a second zero is an octal digit");
    assert_eq!(decimal("0"), Some(0), "a lone zero stays decimal");

    assert_eq!(
        decimal("0x10k"),
        Some(16 * 1024),
        "a suffix applies to a hexadecimal value…"
    );
    assert_eq!(decimal("0x10K"), Some(16 * 1024), "…in either case");
    assert_eq!(decimal("0b101k"), Some(5 * 1024), "…and to a binary one");
    assert_eq!(decimal("010k"), Some(8 * 1024), "…and to an octal one");

    assert_eq!(
        decimal("0x7fffffffffffffff"),
        Some(i64::MAX),
        "the whole range is available in hexadecimal"
    );
    assert!(
        Integer::try_from("0x8000000000000000").is_err(),
        "one above i64::MAX is rejected"
    );
    // `git_parse_signed()` bounds values at `-max - 1` since git 2.50; up to 2.49 the
    // bound was `-max`, which rejected this value. `main` already accepted it in
    // decimal form, so only the hexadecimal spelling is new here.
    assert_eq!(
        decimal("-0x8000000000000000"),
        Some(i64::MIN),
        "including the value whose magnitude Git before 2.50 rejected"
    );
    assert!(
        Integer::try_from("-0x8000000000000001").is_err(),
        "one below i64::MIN is rejected"
    );

    for invalid in ["08", "09", "0x", "0xg", "0b", "0b2", "0o17"] {
        assert!(
            Integer::try_from(invalid).is_err(),
            "`{invalid}` is rejected by git too"
        );
    }

    for prefix in ["0", "0x", "0X", "0b", "0B"] {
        for outer_sign in ["", "+", "-"] {
            for inner_sign in ["+", "-"] {
                for suffix in ["", "k"] {
                    let invalid = format!("{outer_sign}{prefix}{inner_sign}1{suffix}");
                    assert!(
                        Integer::try_from(invalid.as_str()).is_err(),
                        "`{invalid}` is rejected because a sign is only valid before the prefix"
                    );
                }
            }
        }
    }
}
