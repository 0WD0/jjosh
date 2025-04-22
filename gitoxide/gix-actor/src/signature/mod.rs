mod _ref {
    use bstr::ByteSlice;
    use gix_date::Time;
    use winnow::{error::StrContext, prelude::*};

    use crate::{signature::decode, IdentityRef, Signature, SignatureRef};

    impl<'a> SignatureRef<'a> {
        /// Deserialize a signature from the given `data`.
        pub fn from_bytes<E>(mut data: &'a [u8]) -> Result<SignatureRef<'a>, winnow::error::ErrMode<E>>
        where
            E: winnow::error::ParserError<&'a [u8]> + winnow::error::AddContext<&'a [u8], StrContext>,
        {
            decode.parse_next(&mut data)
        }

        /// Create an owned instance from this shared one.
        pub fn to_owned(&self) -> Signature {
            Signature {
                name: self.name.to_owned(),
                email: self.email.to_owned(),
                time: Time::from_bytes(self.time).expect("Time must be valid"),
            }
        }

        /// Trim whitespace surrounding the name and email and return a new signature.
        pub fn trim(&self) -> SignatureRef<'a> {
            SignatureRef {
                name: self.name.trim().as_bstr(),
                email: self.email.trim().as_bstr(),
                time: self.time.trim().as_bstr(),
            }
        }

        /// Return the actor's name and email, effectively excluding the time stamp of this signature.
        pub fn actor(&self) -> IdentityRef<'a> {
            IdentityRef {
                name: self.name,
                email: self.email,
            }
        }
    }
}

mod convert {
    use crate::{Signature, SignatureRef};
    use gix_date::Time;

    impl Signature {
        /// Borrow this instance as immutable
        pub fn to_ref<'a>(&'a self, buf: &'a mut Vec<u8>) -> SignatureRef<'a> {
            SignatureRef {
                name: self.name.as_ref(),
                email: self.email.as_ref(),
                time: self.time.to_ref(buf),
            }
        }
    }

    impl From<SignatureRef<'_>> for Signature {
        fn from(other: SignatureRef<'_>) -> Signature {
            let SignatureRef { name, email, time } = other;
            Signature {
                name: name.to_owned(),
                email: email.to_owned(),
                time: Time::from_bytes(time).expect("Time must be valid"),
            }
        }
    }
}

pub(crate) mod write {
    use bstr::{BStr, ByteSlice};

    use crate::{Signature, SignatureRef};

    /// The Error produced by [`Signature::write_to()`].
    #[derive(Debug, thiserror::Error)]
    #[allow(missing_docs)]
    pub(crate) enum Error {
        #[error(r"Signature name or email must not contain '<', '>' or \n")]
        IllegalCharacter,
    }

    impl From<Error> for std::io::Error {
        fn from(err: Error) -> Self {
            std::io::Error::new(std::io::ErrorKind::Other, err)
        }
    }

    /// Output
    impl Signature {
        /// Serialize this instance to `out` in the git serialization format for actors.
        pub fn write_to(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
            let mut buf = Vec::<u8>::new();
            self.to_ref(&mut buf).write_to(out)
        }
        /// Computes the number of bytes necessary to serialize this signature
        pub fn size(&self) -> usize {
            self.name.len() + 2 /* space <*/ + self.email.len() +  2 /* > space */ + self.time.size()
        }
    }

    impl SignatureRef<'_> {
        /// Serialize this instance to `out` in the git serialization format for actors.
        pub fn write_to(&self, out: &mut dyn std::io::Write) -> std::io::Result<()> {
            out.write_all(validated_token(self.name)?)?;
            out.write_all(b" ")?;
            out.write_all(b"<")?;
            out.write_all(validated_token(self.email)?)?;
            out.write_all(b"> ")?;
            out.write_all(validated_token(self.time)?)
        }
        /// Computes the number of bytes necessary to serialize this signature
        pub fn size(&self) -> usize {
            self.name.len() + 2 /* space <*/ + self.email.len() +  2 /* > space */ + self.time.len()
        }
    }

    pub(crate) fn validated_token(name: &BStr) -> Result<&BStr, Error> {
        if name.find_byteset(b"<>\n").is_some() {
            return Err(Error::IllegalCharacter);
        }
        Ok(name)
    }
}

///
pub mod decode;
pub use decode::function::decode;
