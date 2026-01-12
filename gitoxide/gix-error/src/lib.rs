//! Common error types and utilities for error handling.
//!
//! # Usage
//!
//! * When there is **no callee error** to track, use *simple* `std::error::Error` implementations directly,
//!   via `Result<_, Simple>`.
//! * When there **is callee error to track** *in a `gix-plumbing`*, use `Result<_, Exn<Simple>>`.
//!      - Remember that `Exn<Simple>` does not implement `std::error::Error` so it's not easy to use outside `gix-` crates.
//!      - Use the type-erased version in callbacks like [`Exn`] (without type arguments).
//! * When there **is callee error to track** *in a `gix`*, convert both `std::error::Error` and `Exn<E>` into [`Error`]
//!
#![deny(missing_docs, unsafe_code)]
/// A result type to hide the [Exn] error wrapper.
mod exn;

pub use exn::{ErrorExt, Exn, Frame, OptionExt, ResultExt, Something, Untyped};

/// An error type that wraps an inner type-erased boxed `std::error::Error` or an `Exn` frame.
///
/// In that, it's similar to `anyhow`, but with support for tracking the call site and trees of errors.
///
/// # Warning: `source()` information is stringified and type-erased
///
/// All `source()` values when created with [`Error::from_error()`] are turned into frames,
/// but lose their type information completely.
/// This is because they are only seen as reference and thus can't be stored.
pub struct Error {
    inner: error::Inner,
}

mod error;

mod message;
pub use message::Message;

mod parse;
pub use parse::ParseError;
