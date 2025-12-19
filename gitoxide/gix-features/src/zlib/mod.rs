use std::ffi::c_int;

/// A type to hold all state needed for decompressing a ZLIB encoded stream.
pub struct Decompress(zlib_rs::c_api::z_stream);

// SAFETY: The z_stream contains raw pointers but they are only used through safe zlib-rs APIs
// and the state is fully owned by this struct, making it safe to send across threads.
unsafe impl Send for Decompress {}
unsafe impl Sync for Decompress {}

impl Default for Decompress {
    fn default() -> Self {
        Self::new()
    }
}

impl Decompress {
    /// The amount of bytes consumed from the input so far.
    pub fn total_in(&self) -> u64 {
        self.0.total_in as _
    }

    /// The amount of decompressed bytes that have been written to the output thus far.
    pub fn total_out(&self) -> u64 {
        self.0.total_out as _
    }

    /// Create a new instance. Note that it allocates in various ways and thus should be re-used.
    pub fn new() -> Self {
        let mut stream = zlib_rs::c_api::z_stream::default();
        let config = zlib_rs::inflate::InflateConfig::default();
        zlib_rs::inflate::init(&mut stream, config);
        Self(stream)
    }

    /// Reset the state to allow handling a new stream.
    pub fn reset(&mut self) {
        // SAFETY: Converting from z_stream to InflateStream is safe here as we maintain ownership
        if let Some(stream) = unsafe { zlib_rs::inflate::InflateStream::from_stream_mut(&mut self.0) } {
            zlib_rs::inflate::reset(stream);
        }
    }

    /// Decompress `input` and write all decompressed bytes into `output`, with `flush` defining some details about this.
    pub fn decompress(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        flush: FlushDecompress,
    ) -> Result<Status, DecompressError> {
        self.0.avail_in = input.len() as _;
        self.0.avail_out = output.len() as _;

        self.0.next_in = input.as_ptr();
        self.0.next_out = output.as_mut_ptr();

        // SAFETY: We're converting from the C-compatible z_stream to the typed InflateStream.
        // This is safe because we initialized the stream with `inflate::init` and maintain ownership.
        let stream = unsafe { zlib_rs::inflate::InflateStream::from_stream_mut(&mut self.0) }
            .ok_or(DecompressError::StreamError)?;
        
        let inflate_flush = match flush {
            FlushDecompress::None => zlib_rs::InflateFlush::NoFlush,
            FlushDecompress::Sync => zlib_rs::InflateFlush::SyncFlush,
            FlushDecompress::Finish => zlib_rs::InflateFlush::Finish,
        };
        
        // SAFETY: The zlib_rs::inflate::inflate function is marked unsafe because it operates on
        // raw pointers internally. However, we've properly set up the stream with valid input/output
        // buffers and the actual decompression is done in safe Rust code within zlib-rs.
        match unsafe { zlib_rs::inflate::inflate(stream, inflate_flush) } {
            zlib_rs::ReturnCode::Ok => Ok(Status::Ok),
            zlib_rs::ReturnCode::BufError => Ok(Status::BufError),
            zlib_rs::ReturnCode::StreamEnd => Ok(Status::StreamEnd),
            zlib_rs::ReturnCode::StreamError => Err(DecompressError::StreamError),
            zlib_rs::ReturnCode::DataError => Err(DecompressError::DataError),
            zlib_rs::ReturnCode::MemError => Err(DecompressError::InsufficientMemory),
            err => Err(DecompressError::Unknown { err: err as c_int }),
        }
    }
}

impl Drop for Decompress {
    fn drop(&mut self) {
        // SAFETY: Converting from z_stream to InflateStream is safe here as we maintain ownership
        if let Some(stream) = unsafe { zlib_rs::inflate::InflateStream::from_stream_mut(&mut self.0) } {
            zlib_rs::inflate::end(stream);
        }
    }
}

/// The error produced by [`Decompress::decompress()`].
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
pub enum DecompressError {
    #[error("stream error")]
    StreamError,
    #[error("Not enough memory")]
    InsufficientMemory,
    #[error("Invalid input data")]
    DataError,
    #[error("An unknown error occurred: {err}")]
    Unknown { err: c_int },
}

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

/// non-streaming interfaces for decompression
pub mod inflate {
    /// The error returned by various [Inflate methods][super::Inflate]
    #[derive(Debug, thiserror::Error)]
    #[allow(missing_docs)]
    pub enum Error {
        #[error("Could not write all bytes when decompressing content")]
        WriteInflated(#[from] std::io::Error),
        #[error("Could not decode zip stream, status was '{0}'")]
        Inflate(#[from] super::DecompressError),
        #[error("The zlib status indicated an error, status was '{0:?}'")]
        Status(super::Status),
    }
}

/// Decompress a few bytes of a zlib stream without allocation
#[derive(Default)]
pub struct Inflate {
    /// The actual decompressor doing all the work.
    pub state: Decompress,
}

impl Inflate {
    /// Run the decompressor exactly once. Cannot be run multiple times
    pub fn once(&mut self, input: &[u8], out: &mut [u8]) -> Result<(Status, usize, usize), inflate::Error> {
        let before_in = self.state.total_in();
        let before_out = self.state.total_out();
        let status = self.state.decompress(input, out, FlushDecompress::None)?;
        Ok((
            status,
            (self.state.total_in() - before_in) as usize,
            (self.state.total_out() - before_out) as usize,
        ))
    }

    /// Ready this instance for decoding another data stream.
    pub fn reset(&mut self) {
        self.state.reset();
    }
}

///
pub mod stream;
