/// Support for async packet line writing.
#[cfg(feature = "async-io")]
pub mod async_io;

/// Support for blocking packet line writing.
#[cfg(feature = "blocking-io")]
pub mod blocking_io;
