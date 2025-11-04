use gix_features::progress::Progress;
use gix_transport::Service;
use maybe_async::maybe_async;

#[cfg(feature = "async-client")]
use crate::transport::client::async_io::Transport;
#[cfg(feature = "blocking-client")]
use crate::transport::client::blocking_io::Transport;
use crate::{credentials, handshake::Error, Handshake};

/// Perform a handshake with the server on the other side of `transport`, with `authenticate` being used if authentication
/// turns out to be required. `extra_parameters` are the parameters `(name, optional value)` to add to the handshake,
/// each time it is performed in case authentication is required.
/// `progress` is used to inform about what's currently happening.
#[allow(clippy::result_large_err)]
#[maybe_async]
pub async fn upload_pack<AuthFn, T>(
    transport: T,
    authenticate: AuthFn,
    extra_parameters: Vec<(String, Option<String>)>,
    progress: &mut impl Progress,
) -> Result<Handshake, Error>
where
    AuthFn: FnMut(credentials::helper::Action) -> credentials::protocol::Result,
    T: Transport,
{
    crate::handshake(transport, Service::UploadPack, authenticate, extra_parameters, progress).await
}
