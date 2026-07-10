use crate::{FlushDecompress, Inflate, Status};

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

impl Inflate {
    /// Run the decompressor exactly once. Cannot be run multiple times
    pub fn once(&mut self, input: &[u8], out: &mut [u8]) -> Result<(Status, usize, usize), Error> {
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
