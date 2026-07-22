//! Lean HTTP CONNECT dial helper + reqwest proxy URL builder for optional egress.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("connect timeout")]
    Timeout,
    #[error("proxy rejected CONNECT: {0}")]
    Rejected(String),
    #[error("invalid proxy response")]
    InvalidResponse,
}

#[derive(Debug, Clone)]
pub struct ProxyEndpoint {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Build `http://[user:pass@]host:port` for `reqwest::Proxy::all`.
pub fn reqwest_proxy_url(proxy: &ProxyEndpoint) -> String {
    match (&proxy.username, &proxy.password) {
        (Some(u), Some(p)) => {
            let user = urlencoding_lite(u);
            let pass = urlencoding_lite(p);
            format!("http://{user}:{pass}@{}:{}", proxy.host, proxy.port)
        }
        (Some(u), None) => {
            let user = urlencoding_lite(u);
            format!("http://{user}@{}:{}", proxy.host, proxy.port)
        }
        _ => format!("http://{}:{}", proxy.host, proxy.port),
    }
}

fn urlencoding_lite(s: &str) -> String {
    // Minimal encode for userinfo (space and @ :).
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('@', "%40")
        .replace(':', "%3A")
}

/// Dial `target_host:target_port` via HTTP CONNECT through `proxy`.
pub async fn connect_via_http_proxy(
    proxy: &ProxyEndpoint,
    target_host: &str,
    target_port: u16,
    connect_timeout: Duration,
) -> Result<TcpStream, ConnectError> {
    let dial = async {
        let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port)).await?;
        let mut req = format!(
            "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
        );
        if let (Some(u), Some(p)) = (&proxy.username, &proxy.password) {
            let token = B64.encode(format!("{u}:{p}"));
            req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await?;

        let mut reader = BufReader::new(&mut stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).await?;
        if !status_line.contains("200") {
            return Err(ConnectError::Rejected(status_line.trim().to_string()));
        }
        // drain headers
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            if line == "\r\n" || line == "\n" || line.is_empty() {
                break;
            }
        }
        Ok(stream)
    };
    timeout(connect_timeout, dial)
        .await
        .map_err(|_| ConnectError::Timeout)?
}

pub async fn connect_direct(
    target_host: &str,
    target_port: u16,
    connect_timeout: Duration,
) -> Result<TcpStream, ConnectError> {
    timeout(
        connect_timeout,
        TcpStream::connect((target_host, target_port)),
    )
    .await
    .map_err(|_| ConnectError::Timeout)?
    .map_err(ConnectError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_url_with_auth() {
        let p = ProxyEndpoint {
            host: "proxy.example".into(),
            port: 8080,
            username: Some("u".into()),
            password: Some("p".into()),
        };
        assert_eq!(reqwest_proxy_url(&p), "http://u:p@proxy.example:8080");
    }
}
