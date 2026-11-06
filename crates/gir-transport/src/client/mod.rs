pub mod fetch;
pub mod http;
pub mod push;
pub mod ssh;

use gir_core::{GirError, Repository};

use crate::credentials::RemoteEndpoint;
use crate::refs_discovery::AdvertisedRefs;

pub use fetch::{clone_repository, fetch_from_remote, FetchOutcome, UpdatedTrackingRef};
pub use http::SmartHttpTransport;
pub use push::{push_to_remote, PushOptions, PushOutcome, PushedRefUpdate};
pub use ssh::{LocalTransport, SshTransport};

/// Unified Git Wire Protocol client abstraction across Smart HTTP/HTTPS, SSH, and Local endpoints.
pub trait TransportClient {
    fn discover_upload_pack(&mut self) -> Result<AdvertisedRefs, GirError>;
    fn upload_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError>;
    fn discover_receive_pack(&mut self) -> Result<AdvertisedRefs, GirError>;
    fn receive_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError>;
}

/// Connects to a remote Git endpoint (`https://`, `http://`, `ssh://`, `git@host:path`, `file://`, or local path).
pub fn connect_transport(
    local_repo: Option<&Repository>,
    remote_url: &str,
) -> Result<Box<dyn TransportClient>, GirError> {
    match RemoteEndpoint::parse(remote_url)? {
        RemoteEndpoint::Http {
            url,
            host,
            port,
            path,
            tls,
        } => Ok(Box::new(SmartHttpTransport::new(
            local_repo, url, host, port, path, tls,
        ))),
        RemoteEndpoint::Ssh { user_host, path } => {
            Ok(Box::new(SshTransport::new(user_host, path)))
        }
        RemoteEndpoint::Local { path } => Ok(Box::new(LocalTransport::open(path)?)),
    }
}
