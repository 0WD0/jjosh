use gix_date::Time;

mod baseline;
mod format;
mod parse;

#[test]
fn is_set() {
    assert!(!Time::default().is_set());
    assert!(Time {
        seconds: 1,
        ..Default::default()
    }
    .is_set());
}

mod write_to {
    use bstr::ByteSlice;
    use gix_date::{parse::TimeBuf, SecondsSinceUnixEpoch, Time};

    #[test]
    fn invalid() {
        let time = Time {
            seconds: 0,
            offset: (100 * 60 * 60) + 30 * 60,
        };
        let err = time.write_to(&mut Vec::new()).unwrap_err();
        assert_eq!(err.to_string(), "Cannot represent offsets larger than +-9900");
    }

    #[test]
    fn valid_roundtrips() -> Result<(), Box<dyn std::error::Error>> {
        for (time, expected) in [
            (
                Time {
                    seconds: SecondsSinceUnixEpoch::MAX,
                    offset: 0,
                },
                "9223372036854775807 +0000",
            ),
            (
                Time {
                    seconds: SecondsSinceUnixEpoch::MIN,
                    offset: 0,
                },
                "-9223372036854775808 +0000",
            ),
            (
                Time {
                    seconds: 500,
                    offset: 9000,
                },
                "500 +0230",
            ),
            (
                Time {
                    seconds: 189009009,
                    offset: -36000,
                },
                "189009009 -1000",
            ),
            (Time { seconds: 0, offset: 0 }, "0 +0000"),
            (
                Time {
                    seconds: 0,
                    offset: -24 * 60 * 60,
                },
                "0 -2400",
            ),
            (
                Time {
                    seconds: 0,
                    offset: 24 * 60 * 60,
                },
                "0 +2400",
            ),
            (
                Time {
                    seconds: 0,
                    offset: (25 * 60 * 60) + 30 * 60,
                },
                "0 +2530",
            ),
            (
                Time {
                    seconds: 0,
                    offset: (-25 * 60 * 60) - 30 * 60,
                },
                "0 -2530",
            ),
            (
                Time {
                    seconds: 0,
                    offset: (99 * 60 * 60) + 59 * 60,
                },
                "0 +9959",
            ),
        ] {
            let mut output = Vec::new();
            time.write_to(&mut output)?;
            assert_eq!(output.as_bstr(), expected);
            assert_eq!(time.size(), output.len());

            let actual = output.as_bstr().to_string().parse::<Time>().expect("round-trippable");
            assert_eq!(time, actual);
        }
        Ok(())
    }

    #[test]
    fn max() -> gix_testtools::Result {
        let mut buf = TimeBuf::default();
        Time::MAX.write_to(&mut buf)?;
        assert_eq!(Time::MAX.size(), 25, "The largest possible serialized size");

        let expected = "9223372036854775807 +9959";
        assert_eq!(buf.as_str(), expected);
        assert_eq!(buf.as_str().len(), Time::MAX.size());
        assert_eq!(Time::MAX.to_str(&mut buf), expected);
        Ok(())
    }

    /// This test verifies that issue #2305 is fixed: writing non-UTF8 bytes
    /// should not be allowed to create invalid strings via as_str().
    /// This reproduces the exact PoC from the issue.
    #[test]
    fn issue_2305_cannot_write_non_utf8_bytes() {
        use std::io::Write;
        
        let mut buf = TimeBuf::default();
        // This is the exact PoC from issue #2305
        // Try to write invalid UTF-8 byte (0xff is not valid UTF-8 on its own)
        let result = buf.write(&[0xff]);
        
        // After the fix, this should fail with an error
        assert!(
            result.is_err(),
            "Writing non-UTF8 bytes should fail to prevent unsafe as_str() usage"
        );
        
        // Verify we can still write valid UTF-8
        assert!(buf.write(b"test").is_ok());
        assert_eq!(buf.as_str(), "test");
    }
}
