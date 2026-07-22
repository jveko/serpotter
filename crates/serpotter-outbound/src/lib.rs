//! Lean HTTP CONNECT dial helper for optional proxy egress.

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

/// Dial `target_host:target_port` via HTTP CONNECT through `proxy`.
/// Returns the established TCP stream (caller upgrades TLS if needed).
pub async fn connect_via_http_proxy(
    proxy: &ProxyEndpoint,
    target_host: &str,
    target_port: u16,
    connect_timeout: Duration,
) -> Result<TcpStream, ConnectError> {
    let addr = format!("{}:{}", proxy.host, proxy.port);
    let mut stream = timeout(connect_timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(ConnectError::Io)?;

    let mut req = format!(
        "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\n"
    );
    if let (Some(user), pass) = (&proxy.username, &proxy.password) {
        let token = B64.encode(format!("{user}:{}", pass.as_deref().unwrap_or("")));
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("Proxy-Connection: Keep-Alive\r\n\r\n");

    timeout(connect_timeout, stream.write_all(req.as_bytes()))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(ConnectError::Io)?;

    let mut reader = BufReader::new(&mut stream);
    let mut status_line = String::new();
    timeout(connect_timeout, reader.read_line(&mut status_line))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(ConnectError::Io)?;

    // Drain headers until blank line.
    loop {
        let mut line = String::new();
        timeout(connect_timeout, reader.read_line(&mut line))
            .await
            .map_err(|_| ConnectError::Timeout)?
            .map_err(ConnectError::Io)?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
    }

    // e.g. HTTP/1.1 200 Connection Established
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .map(|c| c.starts_with('2'))
        .unwrap_or(false);
    if !ok {
        return Err(ConnectError::Rejected(status_line.trim().to_string()));
    }

    Ok(stream)
}

/// When no proxy is configured, dial the target host directly.
pub async fn connect_direct(
    target_host: &str,
    target_port: u16,
    connect_timeout: Duration,
) -> Result<TcpStream, ConnectError> {
    let addr = format!("{target_host}:{target_port}");
    timeout(connect_timeout, TcpStream::connect(addr))
        .await
        .map_err(|_| ConnectError::Timeout)?
        .map_err(ConnectError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_connect_localhost_refused_or_ok() {
        // Port 9 is discard; on most systems connection is refused quickly.
        let r = connect_direct("127.0.0.1", 9, Duration::from_millis(200)).await;
        // Either refused (Err) or somehow ok — just exercise the path.
        let _ = r;
    }

    #[test]
    fn proxy_endpoint_clone() {
        let p = ProxyEndpoint {
            host: "proxy.example".into(),
            port: 8080,
            username: Some("u".into()),
            password: Some("p".into()),
        };
        assert_eq!(p.clone().host, "proxy.example");
    }
}
