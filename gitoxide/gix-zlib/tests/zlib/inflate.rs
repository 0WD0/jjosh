use gix_zlib::{DecompressError, Inflate, Status, inflate};

use crate::stream::deflate::compressed;

#[test]
fn once_reports_progress_and_reset_allows_reuse() {
    let input = compressed(b"inflate once");
    let mut inflate = Inflate::default();
    let mut output = [0; 32];

    let (status, consumed, written) = inflate.once(&input, &mut output).expect("valid stream");
    assert_eq!(status, Status::StreamEnd);
    assert_eq!(consumed, input.len());
    assert_eq!(&output[..written], b"inflate once");

    inflate.reset();
    let (_, consumed, written) = inflate.once(&input, &mut output).expect("valid stream after reset");
    assert_eq!(consumed, input.len());
    assert_eq!(written, b"inflate once".len());
}

#[test]
fn errors_expose_their_variants_and_messages() {
    let write = inflate::Error::from(std::io::Error::other("broken writer"));
    assert!(matches!(write, inflate::Error::WriteInflated(_)));
    assert_eq!(
        write.to_string(),
        "Could not write all bytes when decompressing content"
    );

    let decode = inflate::Error::from(DecompressError::DataError);
    assert!(matches!(decode, inflate::Error::Inflate(DecompressError::DataError)));
    assert_eq!(
        decode.to_string(),
        "Could not decode zip stream, status was 'Invalid input data'"
    );

    let status = inflate::Error::Status(Status::BufError);
    assert_eq!(
        status.to_string(),
        "The zlib status indicated an error, status was 'BufError'"
    );
}
