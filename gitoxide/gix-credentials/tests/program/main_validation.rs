use gix_credentials::program::main;
use std::io::Cursor;

#[derive(Debug, thiserror::Error)]
#[error("Test error")]
struct TestError;

#[test]
fn protocol_and_host_without_url_is_valid() {
    let input = b"protocol=https\nhost=github.com\n";
    let mut output = Vec::new();
    
    let result = main(
        ["get".into()],
        Cursor::new(input),
        &mut output,
        |_action, context| -> Result<Option<gix_credentials::protocol::Context>, TestError> {
            // Verify the context has the expected fields
            assert_eq!(context.protocol.as_deref(), Some("https"));
            assert_eq!(context.host.as_deref(), Some("github.com"));
            // Should return None to simulate no credentials found (which is expected in test)
            Ok(None)
        },
    );

    // This should fail because our mock helper returned None (no credentials found)
    // but it should NOT fail because of missing URL
    match result {
        Err(gix_credentials::program::main::Error::CredentialsMissing { .. }) => {
            // This is the expected error - credentials missing, not URL missing
        }
        other => panic!("Expected CredentialsMissing error, got: {:?}", other),
    }
}

#[test]
fn missing_protocol_with_host_fails() {
    let input = b"host=github.com\n";
    let mut output = Vec::new();
    
    let result = main(
        ["get".into()],
        Cursor::new(input),
        &mut output,
        |_action, _context| -> Result<Option<gix_credentials::protocol::Context>, TestError> { Ok(None) },
    );

    match result {
        Err(gix_credentials::program::main::Error::UrlMissing) => {
            // This is expected
        }
        other => panic!("Expected UrlMissing error, got: {:?}", other),
    }
}

#[test]
fn missing_host_with_protocol_fails() {
    let input = b"protocol=https\n";
    let mut output = Vec::new();
    
    let result = main(
        ["get".into()],
        Cursor::new(input),
        &mut output,
        |_action, _context| -> Result<Option<gix_credentials::protocol::Context>, TestError> { Ok(None) },
    );

    match result {
        Err(gix_credentials::program::main::Error::UrlMissing) => {
            // This is expected
        }
        other => panic!("Expected UrlMissing error, got: {:?}", other),
    }
}

#[test]
fn url_alone_is_still_valid() {
    let input = b"url=https://github.com\n";
    let mut output = Vec::new();
    
    let result = main(
        ["get".into()],
        Cursor::new(input),
        &mut output,
        |_action, context| -> Result<Option<gix_credentials::protocol::Context>, TestError> {
            // Verify the context has the expected fields
            assert_eq!(context.url.as_deref().map(|b| &**b), Some("https://github.com".as_bytes()));
            // Should return None to simulate no credentials found (which is expected in test)
            Ok(None)
        },
    );

    // This should fail because our mock helper returned None (no credentials found)
    // but it should NOT fail because of missing URL
    match result {
        Err(gix_credentials::program::main::Error::CredentialsMissing { .. }) => {
            // This is the expected error - credentials missing, not URL missing
        }
        other => panic!("Expected CredentialsMissing error, got: {:?}", other),
    }
}