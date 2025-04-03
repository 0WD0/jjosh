mod from_hex {

    mod valid {
        use gix_hash::ObjectId;

        #[test]
        fn twenty_hex_chars_lowercase() {
            assert!(ObjectId::from_hex(b"1234567890abcdefaaaaaaaaaaaaaaaaaaaaaaaa").is_ok());
        }

        #[test]
        fn twenty_hex_chars_uppercase() {
            assert!(ObjectId::from_hex(b"1234567890ABCDEFAAAAAAAAAAAAAAAAAAAAAAAA").is_ok());
        }
    }

    mod invalid {
        use gix_hash::{decode, ObjectId};

        #[test]
        fn non_hex_characters() {
            assert!(matches!(
                ObjectId::from_hex(b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").unwrap_err(),
                decode::Error::Invalid
            ));
        }

        #[test]
        fn too_short() {
            assert!(matches!(
                ObjectId::from_hex(b"abcd").unwrap_err(),
                decode::Error::InvalidHexEncodingLength(4)
            ));
        }
        #[test]
        fn too_long() {
            assert!(matches!(
                ObjectId::from_hex(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaf").unwrap_err(),
                decode::Error::InvalidHexEncodingLength(41)
            ));
        }
    }
}

mod sha1 {
    use std::str::FromStr as _;

    use gix_hash::{hasher, Kind, ObjectId};

    fn hash_contents(s: &[u8]) -> Result<ObjectId, hasher::Error> {
        let mut hasher = hasher(Kind::Sha1);
        hasher.update(s);
        hasher.try_finalize()
    }

    #[test]
    fn empty_blob() {
        assert_eq!(
            ObjectId::empty_blob(Kind::Sha1),
            hash_contents(b"blob 0\0").expect("empty blob to not collide"),
        );
    }

    #[test]
    fn empty_tree() {
        assert_eq!(
            ObjectId::empty_tree(Kind::Sha1),
            hash_contents(b"tree 0\0").expect("empty tree to not collide"),
        );
    }

    /// Check the test vectors from RFC 3174.
    #[test]
    fn rfc_3174() {
        let fixtures: &[(&[u8], &str)] = &[
            (b"abc", "A9 99 3E 36 47 06 81 6A BA 3E 25 71 78 50 C2 6C 9C D0 D8 9D"),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "84 98 3E 44 1C 3B D2 6E BA AE 4A A1 F9 51 29 E5 E5 46 70 F1",
            ),
            (
                &b"a".repeat(1000000),
                "34 AA 97 3C D4 C4 DA A4 F6 1E EB 2B DB AD 27 31 65 34 01 6F",
            ),
            (
                &b"0123456701234567012345670123456701234567012345670123456701234567".repeat(10),
                "DE A3 56 A2 CD DD 90 C7 A7 EC ED C5 EB B5 63 93 4F 46 04 52",
            ),
        ];
        for (input, output) in fixtures {
            assert_eq!(
                hash_contents(input).expect("RFC inputs to not collide"),
                ObjectId::from_str(&output.to_lowercase().replace(' ', "")).expect("RFC digests to be valid"),
            );
        }
    }

    /// Check the “SHA‐1 is a Shambles” chosen‐prefix collision.
    ///
    /// See <https://sha-mbles.github.io/>.
    ///
    /// We test these and not the earlier SHAttered PDFs because they are much smaller.
    #[test]
    fn shambles() {
        let message_a = include_bytes!("../fixtures/shambles/messageA");
        let message_b = include_bytes!("../fixtures/shambles/messageB");
        assert_ne!(message_a, message_b);

        let expected =
            ObjectId::from_str("8ac60ba76f1999a1ab70223f225aefdc78d4ddc0").expect("Shambles digest to be valid");

        let Err(hasher::Error::CollisionAttack { digest }) = hash_contents(message_a) else {
            panic!("expected Shambles input to collide");
        };
        assert_eq!(digest, expected);

        let Err(hasher::Error::CollisionAttack { digest }) = hash_contents(message_b) else {
            panic!("expected Shambles input to collide");
        };
        assert_eq!(digest, expected);
    }
}
