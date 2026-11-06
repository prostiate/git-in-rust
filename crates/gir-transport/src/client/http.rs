use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpStream;
use std::process::{Command, Stdio};

use gir_core::{GirError, Repository};

use crate::client::TransportClient;
use crate::credentials::{query_credential_helper, Credentials};
use crate::pktline::PktLineReader;
use crate::refs_discovery::{parse_advertised_refs, AdvertisedRefs};

/// Smart HTTP / HTTPS Git Wire Protocol client (`git-upload-pack` and `git-receive-pack`).
pub struct SmartHttpTransport {
    url: String,
    host: String,
    port: u16,
    path: String,
    tls: bool,
    creds: Option<Credentials>,
}

impl SmartHttpTransport {
    pub fn new(
        local_repo: Option<&Repository>,
        url: String,
        host: String,
        port: u16,
        path: String,
        tls: bool,
    ) -> Self {
        let creds = query_credential_helper(local_repo, tls, &host, &path);
        Self {
            url,
            host,
            port,
            path,
            tls,
            creds,
        }
    }

    fn http_get(&self, service: &str) -> Result<Vec<u8>, GirError> {
        let full_url = format!("{}/info/refs?service={service}", self.url);
        if self.tls {
            self.curl_request("GET", &full_url, None, None)
        } else {
            let req_path = format!("{}/info/refs?service={service}", self.path);
            self.tcp_http_request("GET", &req_path, None, None, &[])
        }
    }

    fn http_post(&self, service: &str, body: &[u8]) -> Result<Vec<u8>, GirError> {
        let full_url = format!("{}/{service}", self.url);
        let content_type = format!("application/x-{service}-request");
        let accept = format!("application/x-{service}-result");

        if self.tls {
            self.curl_request("POST", &full_url, Some(&content_type), Some((&accept, body)))
        } else {
            let req_path = format!("{}/{service}", self.path);
            self.tcp_http_request("POST", &req_path, Some(&content_type), Some(&accept), body)
        }
    }

    fn tcp_http_request(
        &self,
        method: &str,
        req_path: &str,
        content_type: Option<&str>,
        accept: Option<&str>,
        body: &[u8],
    ) -> Result<Vec<u8>, GirError> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))?;
        let mut req_hdr = format!(
            "{method} {req_path} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: gir/0.1.0\r\nConnection: close\r\n",
            self.host, self.port
        );
        if let Some(ct) = content_type {
            req_hdr.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        if let Some(acc) = accept {
            req_hdr.push_str(&format!("Accept: {acc}\r\n"));
        }
        if let Some(ref c) = self.creds {
            req_hdr.push_str(&format!("Authorization: {}\r\n", c.basic_auth_header()));
        }
        if !body.is_empty() || method == "POST" {
            req_hdr.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        req_hdr.push_str("\r\n");

        stream.write_all(req_hdr.as_bytes())?;
        if !body.is_empty() {
            stream.write_all(body)?;
        }
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line)?;
        if !status_line.contains(" 200 ") {
            return Err(GirError::InvalidArgument(format!(
                "HTTP request failed: {}",
                status_line.trim()
            )));
        }

        let mut chunked = false;
        let mut content_len: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line)?;
            if n == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            } else if let Some(val) = lower.strip_prefix("content-length:") {
                content_len = val.trim().parse().ok();
            }
        }

        if chunked {
            read_chunked_body(&mut reader)
        } else if let Some(len) = content_len {
            let mut out = vec![0u8; len];
            reader.read_exact(&mut out)?;
            Ok(out)
        } else {
            let mut out = Vec::new();
            reader.read_to_end(&mut out)?;
            Ok(out)
        }
    }

    fn curl_request(
        &self,
        method: &str,
        full_url: &str,
        content_type: Option<&str>,
        post_data: Option<(&str, &[u8])>,
    ) -> Result<Vec<u8>, GirError> {
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "--fail", "-L", "-A", "gir/0.1.0", "-X", method]);

        if let Some(ct) = content_type {
            cmd.arg("-H").arg(format!("Content-Type: {ct}"));
        }
        if let Some((accept, _)) = post_data {
            cmd.arg("-H").arg(format!("Accept: {accept}"));
            cmd.args(["--data-binary", "@-"]);
        }
        if let Some(ref c) = self.creds {
            cmd.arg("-H")
                .arg(format!("Authorization: {}", c.basic_auth_header()));
        }
        cmd.arg(full_url);

        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some((_, body)) = post_data {
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(body)?;
            }
        }
        drop(child.stdin.take());

        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(GirError::InvalidArgument(format!(
                "Smart HTTP request to '{full_url}' failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }
}

impl TransportClient for SmartHttpTransport {
    fn discover_upload_pack(&mut self) -> Result<AdvertisedRefs, GirError> {
        let bytes = self.http_get("git-upload-pack")?;
        let mut reader = PktLineReader::new(Cursor::new(bytes));
        parse_advertised_refs(&mut reader, Some("git-upload-pack"))
    }

    fn upload_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        self.http_post("git-upload-pack", request_bytes)
    }

    fn discover_receive_pack(&mut self) -> Result<AdvertisedRefs, GirError> {
        let bytes = self.http_get("git-receive-pack")?;
        let mut reader = PktLineReader::new(Cursor::new(bytes));
        parse_advertised_refs(&mut reader, Some("git-receive-pack"))
    }

    fn receive_pack(&mut self, request_bytes: &[u8]) -> Result<Vec<u8>, GirError> {
        self.http_post("git-receive-pack", request_bytes)
    }
}

fn read_chunked_body<R: BufRead>(reader: &mut R) -> Result<Vec<u8>, GirError> {
    let mut out = Vec::new();
    loop {
        let mut size_line = String::new();
        reader.read_line(&mut size_line)?;
        let hex_part = size_line
            .trim()
            .split(';')
            .next()
            .unwrap_or("0");
        let chunk_len = usize::from_str_radix(hex_part, 16).map_err(|_| {
            GirError::InvalidArgument(format!("invalid HTTP chunk length '{hex_part}'"))
        })?;
        if chunk_len == 0 {
            let mut trailer = String::new();
            let _ = reader.read_line(&mut trailer);
            break;
        }
        let prev_len = out.len();
        out.resize(prev_len + chunk_len, 0);
        reader.read_exact(&mut out[prev_len..])?;
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf)?;
    }
    Ok(out)
}
