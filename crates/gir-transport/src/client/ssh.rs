use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use gir_core::{GirError, Repository};

use crate::client::TransportClient;
use crate::pktline::PktLineReader;
use crate::refs_discovery::{parse_advertised_refs, AdvertisedRefs};
use crate::server::{
    advertise_receive_pack_refs, advertise_upload_pack_refs, handle_receive_pack,
    handle_upload_pack,
};

/// Local repository wire transport that connects the client directly to `gir-transport::server`
/// `upload-pack` and `receive-pack` wire handlers over in-memory byte streams.
pub struct LocalTransport {
    remote_repo: Repository,
}

impl LocalTransport {
    pub fn open(path: PathBuf) -> Result<Self, GirError> {
        let remote_repo = Repository::discover(&path)?;
        Ok(Self { remote_repo })
    }
}

impl TransportClient for LocalTransport {
    fn discover_upload_pack(&mut self) -> Result<AdvertisedRefs, GirError> {
        let adv_bytes = advertise_upload_pack_refs(&self.remote_repo, true)?;
        let mut reader = PktLineReader::new(Cursor::new(adv_bytes));
        parse_advertised_refs(&mut reader, Some("git-upload-pack"))
    }

    fn upload_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        handle_upload_pack(&self.remote_repo, request_bytes)
    }

    fn discover_receive_pack(&mut self) -> Result<AdvertisedRefs, GirError> {
        let adv_bytes = advertise_receive_pack_refs(&self.remote_repo, true)?;
        let mut reader = PktLineReader::new(Cursor::new(adv_bytes));
        parse_advertised_refs(&mut reader, Some("git-receive-pack"))
    }

    fn receive_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        handle_receive_pack(&self.remote_repo, request_bytes)
    }
}

/// SSH Git Wire Protocol client (`ssh <user@host> git-upload-pack '<path>'` / `git-receive-pack '<path>'`).
pub struct SshTransport {
    user_host: String,
    path: String,
    active_child: Option<Child>,
}

impl SshTransport {
    pub fn new(user_host: String, path: String) -> Self {
        Self {
            user_host,
            path,
            active_child: None,
        }
    }

    fn spawn_service(&mut self, service: &str) -> Result<AdvertisedRefs, GirError> {
        let escaped_path = self.path.replace('\'', "'\\''");
        let remote_cmd = format!("{service} '{escaped_path}'");
        let mut child = if let Ok(ssh_cmd) = std::env::var("GIT_SSH_COMMAND") {
            let full_sh = format!("{ssh_cmd} \"{}\" \"{remote_cmd}\"", self.user_host);
            Command::new("sh")
                .arg("-c")
                .arg(full_sh)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?
        } else {
            let ssh_bin = std::env::var("GIT_SSH").unwrap_or_else(|_| "ssh".to_string());
            Command::new(ssh_bin)
                .arg(&self.user_host)
                .arg(remote_cmd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?
        };

        let stdout = child.stdout.as_mut().ok_or_else(|| {
            GirError::InvalidArgument("failed to capture ssh stdout".to_string())
        })?;
        let mut reader = PktLineReader::new(stdout);
        let adv = parse_advertised_refs(&mut reader, None)?;
        self.active_child = Some(child);
        Ok(adv)
    }

    fn exchange_with_active(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        let mut child = self.active_child.take().ok_or_else(|| {
            GirError::InvalidArgument("SSH service not initialized before exchange".to_string())
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(request_bytes)?;
            stdin.flush()?;
        }

        let mut response = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            stdout.read_to_end(&mut response)?;
        }

        let status = child.wait()?;
        if !status.success() && response.is_empty() {
            return Err(GirError::InvalidArgument(format!(
                "SSH transport exited with status {status}"
            )));
        }
        Ok(response)
    }
}

impl TransportClient for SshTransport {
    fn discover_upload_pack(&mut self) -> Result<AdvertisedRefs, GirError> {
        self.spawn_service("git-upload-pack")
    }

    fn upload_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        self.exchange_with_active(request_bytes)
    }

    fn discover_receive_pack(&mut self) -> Result<AdvertisedRefs, GirError> {
        self.spawn_service("git-receive-pack")
    }

    fn receive_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        self.exchange_with_active(request_bytes)
    }
}
