//! Jellyfin on the Syncplay port: a streaming reverse proxy to the local
//! Jellyfin, so a friend who reaches the Syncplay server also reaches the
//! shared Jellyfin without another open port. WebSocket upgrades are tunnelled.
//! Jellyfin trusts the `X-Forwarded-*` headers because 127.0.0.1 is in its
//! `KnownProxies` (see `netconfig`).

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use parking_lot::RwLock;
use std::net::IpAddr;
use std::time::Duration;
use tracing::debug;

pub struct JellyfinProxy {
    client: reqwest::Client,
    target: RwLock<Option<u16>>,
}

impl Default for JellyfinProxy {
    fn default() -> Self {
        Self::new()
    }
}

const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Headers that must not be forwarded: hop-by-hop ones, and any the
/// `Connection` header names.
fn strip_hop_by_hop(headers: &HeaderMap, keep_upgrade: bool) -> HeaderMap {
    let named: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    let mut out = HeaderMap::new();
    for (k, v) in headers {
        let name = k.as_str();
        let upgrade_header = keep_upgrade && (name == "connection" || name == "upgrade");
        let hop = HOP_BY_HOP.contains(&name) || named.iter().any(|n| n == name);
        if upgrade_header || !hop {
            out.append(k.clone(), v.clone());
        }
    }
    out
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE)
        && headers
            .get_all(header::CONNECTION)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| {
                v.split(',')
                    .any(|t| t.trim().eq_ignore_ascii_case("upgrade"))
            })
}

fn text(status: StatusCode, body: &str) -> Response<Body> {
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = status;
    r
}

impl JellyfinProxy {
    pub fn new() -> Self {
        let client = crate::net::client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            client,
            target: RwLock::new(None),
        }
    }

    /// The local Jellyfin HTTP port while sharing is on; None turns the proxy off.
    pub fn set_target(&self, port: Option<u16>) {
        *self.target.write() = port;
    }

    pub fn target(&self) -> Option<u16> {
        *self.target.read()
    }

    pub async fn forward(&self, mut req: Request<Body>, peer: IpAddr, tls: bool) -> Response<Body> {
        let Some(port) = self.target() else {
            return text(StatusCode::NOT_FOUND, "Not found");
        };
        let path = req
            .uri()
            .path_and_query()
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| "/".into());
        let url = format!("http://127.0.0.1:{port}{path}");
        let upgrade = is_upgrade(req.headers());
        let mut headers = strip_hop_by_hop(req.headers(), upgrade);
        let host = headers.remove(header::HOST);
        // Only the real peer address goes upstream, so a client can't claim to be on the LAN.
        let forwarded_for = peer.to_string();
        headers.remove("x-forwarded-for");
        headers.remove("x-forwarded-proto");
        headers.remove("x-forwarded-host");
        headers.remove("x-real-ip");
        headers.remove("forwarded");
        if let Ok(v) = HeaderValue::from_str(&forwarded_for) {
            headers.insert(HeaderName::from_static("x-forwarded-for"), v);
        }
        headers.insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static(if tls { "https" } else { "http" }),
        );
        if let Some(h) = host {
            headers.insert(HeaderName::from_static("x-forwarded-host"), h);
        }

        let on_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
        let method = req.method().clone();
        let body = req.into_body();
        let mut rb = self.client.request(method, url).headers(headers);
        if !upgrade {
            rb = rb.body(reqwest::Body::wrap_stream(body.into_data_stream()));
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => {
                debug!(error = %e, "Jellyfin proxy: upstream unreachable");
                return text(StatusCode::BAD_GATEWAY, "Jellyfin is not reachable");
            }
        };
        let status = resp.status();
        let mut out_headers =
            strip_hop_by_hop(resp.headers(), status == StatusCode::SWITCHING_PROTOCOLS);

        if status == StatusCode::SWITCHING_PROTOCOLS {
            let Some(on_upgrade) = on_upgrade else {
                return text(StatusCode::BAD_GATEWAY, "unexpected upgrade");
            };
            tokio::spawn(async move {
                let (Ok(client), Ok(upstream)) = (on_upgrade.await, resp.upgrade().await) else {
                    return;
                };
                let mut client = hyper_util::rt::TokioIo::new(client);
                let mut upstream = upstream;
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
            let mut r = Response::new(Body::empty());
            *r.status_mut() = status;
            std::mem::swap(r.headers_mut(), &mut out_headers);
            return r;
        }

        let mut r = Response::new(Body::from_stream(resp.bytes_stream()));
        *r.status_mut() = status;
        std::mem::swap(r.headers_mut(), &mut out_headers);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_by_hop_headers_are_dropped() {
        let mut h = HeaderMap::new();
        h.insert(
            "connection",
            HeaderValue::from_static("keep-alive, x-secret"),
        );
        h.insert("x-secret", HeaderValue::from_static("1"));
        h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        h.insert("x-emby-token", HeaderValue::from_static("abc"));
        h.insert("transfer-encoding", HeaderValue::from_static("chunked"));
        let out = strip_hop_by_hop(&h, false);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("x-emby-token"));
        assert!(!is_upgrade(&h));

        let mut ws = HeaderMap::new();
        ws.insert("connection", HeaderValue::from_static("Upgrade"));
        ws.insert("upgrade", HeaderValue::from_static("websocket"));
        assert!(is_upgrade(&ws));
        assert_eq!(strip_hop_by_hop(&ws, true).len(), 2);
    }
}
