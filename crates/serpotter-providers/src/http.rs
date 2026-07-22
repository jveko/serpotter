use std::time::Duration;

use reqwest::Client;

pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Build a reqwest client with connect/request timeouts.
/// When `proxy_url` is `Some`, attaches that proxy if it parses; invalid proxy falls through
/// without proxy. Build failure falls back to a non-proxied client with the same timeouts.
pub fn build_http(proxy_url: Option<&str>) -> Client {
    let mut b = Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT);
    if let Some(p) = proxy_url {
        if let Ok(proxy) = reqwest::Proxy::all(p) {
            b = b.proxy(proxy);
        }
    }
    b.build().unwrap_or_else(|_| {
        Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client")
    })
}
