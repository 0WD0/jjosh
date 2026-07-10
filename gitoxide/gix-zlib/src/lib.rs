#![deny(missing_docs)]
//! Streaming compression and decompression utilities used by gitoxide.

/// A type to hold all state needed for decompressing a ZLIB encoded stream.
pub struct Decompress(zlib_rs::Inflate);

/// The status returned by [`Decompress::decompress()`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The decompress operation went well. Not to be confused with `StreamEnd`, so one can continue
    /// the decompression.
    Ok,
    /// An error occurred when decompression.
    BufError,
    /// The stream was fully decompressed.
    StreamEnd,
}

/// Values which indicate the form of flushing to be used when
/// decompressing in-memory data.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
#[allow(clippy::unnecessary_cast)]
pub enum FlushDecompress {
    /// A typical parameter for passing to compression/decompression functions,
    /// this indicates that the underlying stream to decide how much data to
    /// accumulate before producing output in order to maximize compression.
    None = 0,

    /// All pending output is flushed to the output buffer and the output is
    /// aligned on a byte boundary so that the decompressor can get all input
    /// data available so far.
    ///
    /// Flushing may degrade compression for some compression algorithms and so
    /// it should only be used when necessary. This will complete the current
    /// deflate block and follow it with an empty stored block.
    Sync = 2,

    /// Pending input is processed and pending output is flushed.
    ///
    /// The return value may indicate that the stream is not yet done and more
    /// data has yet to be processed.
    Finish = 4,
}

/// Decompress a few bytes of a zlib stream without allocation
#[derive(Default)]
pub struct Inflate {
    /// The actual decompressor doing all the work.
    pub state: Decompress,
}

/// Streaming compression and decompression utilities built on [`std::io`] traits.
pub mod stream;

/// Types supporting single-step, allocation-free decompression.
pub mod inflate;

mod decompress;
pub use decompress::DecompressError;
