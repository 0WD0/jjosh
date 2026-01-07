//! Common error types and utilities for error handling.
#![deny(missing_docs, unsafe_code)]

pub use exn::Exn;
/// A result type to hide the [Exn] error wrapper.
pub use exn::Result;
pub use exn::ResultExt;
