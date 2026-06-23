//! Regression tests specific to the blocking `reqwest` HTTP backend.

use std::error::Error;

use gix_transport::{
    Protocol, Service,
    client::blocking_io::{Transport, http},
};

/// Regression for <https://github.com/GitoxideLabs/gitoxide/issues/2140>: when a request fails
/// without an HTTP status (for example a connection or TLS failure), the underlying error must be
/// kept as `source()` instead of being stringified, so callers can see the real cause.
#[test]
fn request_failure_without_status_preserves_error_source() {
    // Bind then immediately drop a listener so the port reliably refuses connections: a failure
    // with no HTTP status, exercising the path that previously stringified the error.
    let addr = {
        let server = std::net::TcpListener::bind("127.0.0.1:0").expect("can bind an ephemeral port");
        server.local_addr().expect("listener has a local address")
    };

    let url = format!("http://{addr}/repo");
    let mut client =
        http::connect::<http::reqwest::Remote>(url.as_str().try_into().expect("the url is valid"), Protocol::V1, false);

    let error = client
        .handshake(Service::UploadPack, &[])
        .err()
        .expect("a refused connection must produce an error");
    let io_error = error
        .source()
        .and_then(|source| source.downcast_ref::<std::io::Error>())
        .expect("the transport error wraps an io::Error");
    assert!(
        io_error.source().is_some(),
        "the underlying error must be preserved as source(), not stringified: {io_error:?}"
    );
}
