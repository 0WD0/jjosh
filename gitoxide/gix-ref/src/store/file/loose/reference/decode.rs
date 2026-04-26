use gix_hash::ObjectId;
use gix_object::bstr::BString;

use crate::{parse::hex_hash, store_impl::file::loose::Reference, FullName, Target};

enum MaybeUnsafeState {
    Id(ObjectId),
    UnvalidatedPath(BString),
}

/// The error returned by [`Reference::try_from_path()`].
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum Error {
    #[error("{content:?} could not be parsed")]
    Parse { content: BString },
    #[error("The path {path:?} to a symbolic reference within a ref file is invalid")]
    RefnameValidation {
        source: gix_validate::reference::name::Error,
        path: BString,
    },
}

impl TryFrom<MaybeUnsafeState> for Target {
    type Error = Error;

    fn try_from(v: MaybeUnsafeState) -> Result<Self, Self::Error> {
        Ok(match v {
            MaybeUnsafeState::Id(id) => Target::Object(id),
            MaybeUnsafeState::UnvalidatedPath(name) => {
                Target::Symbolic(match gix_validate::reference::name(name.as_ref()) {
                    Ok(_) => FullName(name),
                    Err(err) => {
                        return Err(Error::RefnameValidation {
                            source: err,
                            path: name,
                        })
                    }
                })
            }
        })
    }
}

impl Reference {
    /// Create a new reference of the given `parent` store with `relative_path` service as unique identifier
    /// at which the `path_contents` was read to obtain the refs value.
    pub fn try_from_path(name: FullName, path_contents: &[u8]) -> Result<Self, Error> {
        Ok(Reference {
            name,
            target: parse(path_contents)
                .map_err(|_| Error::Parse {
                    content: path_contents.into(),
                })?
                .try_into()?,
        })
    }
}

/// Parse the contents of a loose reference file.
///
/// A *symbolic* reference starts with `ref: `, may have additional spaces before
/// the path, and returns [`MaybeUnsafeState::UnvalidatedPath`] with the path
/// bytes up to the next line ending or the end of input. The path is validated
/// later when it is converted into a [`Target`].
///
/// A *direct* reference starts with a hexadecimal object id and returns
/// [`MaybeUnsafeState::Id`].
///
/// If neither reference form can be parsed, an error is returned.
fn parse(mut i: &[u8]) -> Result<MaybeUnsafeState, ()> {
    if let Some(rest) = i.strip_prefix(b"ref: ") {
        i = rest;
        while i.first() == Some(&b' ') {
            i = &i[1..];
        }
        let path_end = i.iter().position(|b| *b == b'\r' || *b == b'\n').unwrap_or(i.len());
        let path = i[..path_end].into();
        Ok(MaybeUnsafeState::UnvalidatedPath(path))
    } else {
        let hex = hex_hash(&mut i)?;
        Ok(MaybeUnsafeState::Id(ObjectId::from_hex(hex).expect("prior validation")))
    }
}
