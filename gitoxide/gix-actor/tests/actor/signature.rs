mod write_to {
    mod invalid {
        use gix_actor::Signature;
        use gix_date::Time;

        #[test]
        fn name() {
            let signature = Signature {
                name: "invalid < middlename".into(),
                email: "ok".into(),
                time: Time::default(),
            };
            insta::assert_debug_snapshot!(signature.write_to(&mut Vec::new()).expect_err("the signature is invalid"), "signature names reject angle brackets", @r#"
            Custom {
                kind: Other,
                error: Signature name or email must not contain '<', '>' or \n, input="invalid < middlename",
            }
            "#);
        }

        #[test]
        fn email() {
            let signature = Signature {
                name: "ok".into(),
                email: "server>.example.com".into(),
                time: Time::default(),
            };
            insta::assert_debug_snapshot!(signature.write_to(&mut Vec::new()).expect_err("the signature is invalid"), "signature email addresses reject angle brackets", @r#"
            Custom {
                kind: Other,
                error: Signature name or email must not contain '<', '>' or \n, input="server>.example.com",
            }
            "#);
        }

        #[test]
        fn name_with_newline() {
            let signature = Signature {
                name: "hello\nnewline".into(),
                email: "name@example.com".into(),
                time: Time::default(),
            };
            insta::assert_debug_snapshot!(signature.write_to(&mut Vec::new()).expect_err("the signature is invalid"), "signature names reject newlines", @r#"
            Custom {
                kind: Other,
                error: Signature name or email must not contain '<', '>' or \n, input="hello\nnewline",
            }
            "#);
        }
    }
}

use bstr::ByteSlice;
use gix_actor::{Signature, SignatureRef};

#[test]
fn trim() {
    let sig = gix_actor::SignatureRef::from_bytes(b" \t hello there \t < \t email \t > 1 -0030").unwrap();
    let sig = sig.trim();
    assert_eq!(sig.name, "hello there");
    assert_eq!(sig.email, "email");
}

#[test]
fn round_trip() -> gix_testtools::TestResult {
    static DEFAULTS: &[&[u8]] =     &[
        b"Sebastian Thiel <byronimo@gmail.com> 1 -0030",
        b"Sebastian Thiel <byronimo@gmail.com> -1500 -0030",
        ".. ☺️Sebastian 王知明 Thiel🙌 .. <byronimo@gmail.com> 1528473343 +0230".as_bytes(),
        b".. whitespace  \t  is explicitly allowed    - unicode aware trimming must be done elsewhere  <byronimo@gmail.com> 1528473343 +0230"
    ];

    for input in DEFAULTS {
        let signature: Signature = gix_actor::SignatureRef::from_bytes(input)?.into();
        let mut output = Vec::new();
        signature.write_to(&mut output)?;
        assert_eq!(output.as_bstr(), input.as_bstr());
    }
    Ok(())
}

#[test]
fn signature_ref_round_trips_with_seconds_in_offset() -> gix_testtools::TestResult {
    let input = b"Sebastian Thiel <byronimo@gmail.com> 1313584730 +051800"; // Seen in the wild
    let signature: SignatureRef = gix_actor::SignatureRef::from_bytes(input)?;
    let mut output = Vec::new();
    signature.write_to(&mut output)?;
    assert_eq!(output.as_bstr(), input.as_bstr());
    Ok(())
}

#[test]
fn parse_timestamp_with_trailing_digits() -> gix_testtools::TestResult {
    let signature = gix_actor::SignatureRef::from_bytes(b"first last <name@example.com> 1312735823 +051800")?;
    assert_eq!(
        signature,
        SignatureRef {
            name: "first last".into(),
            email: "name@example.com".into(),
            time: "1312735823 +051800",
        }
    );

    let signature = gix_actor::SignatureRef::from_bytes(b"first last <name@example.com> 1312735823 +0518")?;
    assert_eq!(
        signature,
        SignatureRef {
            name: "first last".into(),
            email: "name@example.com".into(),
            time: "1312735823 +0518",
        }
    );
    Ok(())
}

#[test]
fn parse_missing_timestamp() -> gix_testtools::TestResult {
    let signature = gix_actor::SignatureRef::from_bytes(b"first last <name@example.com>")?;
    assert_eq!(
        signature,
        SignatureRef {
            name: "first last".into(),
            email: "name@example.com".into(),
            time: ""
        }
    );
    Ok(())
}
