//! One port for everything: the first byte a client sends on the Syncplay
//! port says what it speaks. `{` is Syncplay (JSON lines, maybe STARTTLS),
//! `0x16` is a TLS handshake for HTTPS, and an uppercase ASCII letter starts
//! an HTTP request. HTTP connections are handed to an axum router through a
//! channel-backed [`Listener`]: the relay endpoints, a probe, and the shared
//! Jellyfin for every other path. Vanilla mode never sniffs.

use super::room::{ConnId, ServerState};
use super::server::Extensions;
use crate::relay::scheduler::Mode;
use axum::body::Body;
use axum::extract::connect_info::Connected;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Response, StatusCode};
use axum::middleware::Next;
use axum::routing::{get, put};
use axum::serve::{IncomingStream, Listener};
use axum::Router;
use parking_lot::Mutex;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Weak;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniff {
    Syncplay,
    Tls,
    Http,
}

pub fn sniff(first: u8) -> Sniff {
    match first {
        0x16 => Sniff::Tls,
        b'A'..=b'Z' => Sniff::Http,
        _ => Sniff::Syncplay,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConnMeta {
    pub peer: SocketAddr,
    pub tls: bool,
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type BoxIo = Box<dyn Io>;
pub type HttpSender = mpsc::Sender<(BoxIo, ConnMeta)>;

pub struct MuxListener {
    rx: mpsc::Receiver<(BoxIo, ConnMeta)>,
    local: ConnMeta,
}

impl Listener for MuxListener {
    type Io = BoxIo;
    type Addr = ConnMeta;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.rx.recv().await {
            Some(conn) => conn,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}

impl Connected<IncomingStream<'_, MuxListener>> for ConnMeta {
    fn connect_info(stream: IncomingStream<'_, MuxListener>) -> Self {
        *stream.remote_addr()
    }
}

#[derive(Clone)]
struct Ctx {
    state: Weak<Mutex<ServerState>>,
    ext: Extensions,
}

impl Ctx {
    fn vanilla(&self) -> bool {
        self.state.upgrade().is_none_or(|s| s.lock().vanilla())
    }

    /// The session behind `Authorization: Bearer <token>` or `?t=<token>`.
    fn session(&self, headers: &HeaderMap, q: &Q) -> Option<(ConnId, String)> {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        let token = bearer.or(q.t.as_deref())?;
        self.state.upgrade()?.lock().session_for_token(token)
    }
}

#[derive(Debug, Default, Deserialize)]
struct Q {
    t: Option<String>,
    mode: Option<String>,
}

fn reply(code: StatusCode, msg: &str) -> Response<Body> {
    let mut r = Response::new(Body::from(msg.to_string()));
    *r.status_mut() = code;
    r
}

/// Start the HTTP side; connections arrive through the returned sender.
pub fn start_http(
    state: Weak<Mutex<ServerState>>,
    ext: Extensions,
    local: SocketAddr,
) -> (HttpSender, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(64);
    let ctx = Ctx { state, ext };
    let app = Router::new()
        .route("/yarmiplay/info", get(info))
        .route("/yarmiplay/files/{id}", get(file))
        .route("/yarmiplay/upload/{id}", put(upload))
        .fallback(proxy)
        .layer(axum::middleware::from_fn_with_state(ctx.clone(), guard))
        .layer(DefaultBodyLimit::disable())
        .with_state(ctx)
        .into_make_service_with_connect_info::<ConnMeta>();
    let listener = MuxListener {
        rx,
        local: ConnMeta {
            peer: local,
            tls: false,
        },
    };
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (tx, task)
}

/// Connections kept alive from before vanilla mode was switched on get nothing.
async fn guard(State(ctx): State<Ctx>, req: Request, next: Next) -> Response<Body> {
    if ctx.vanilla() {
        let mut r = reply(StatusCode::NOT_FOUND, "Not found");
        r.headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
        return r;
    }
    next.run(req).await
}

async fn info(State(ctx): State<Ctx>) -> Response<Body> {
    let Some(state) = ctx.state.upgrade() else {
        return reply(StatusCode::NOT_FOUND, "Not found");
    };
    let v = state.lock().ext_info();
    let mut r = Response::new(Body::from(v.to_string()));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

async fn file(
    State(ctx): State<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<Q>,
    method: Method,
    headers: HeaderMap,
) -> Response<Body> {
    let Some(relay) = ctx.ext.relay.as_ref().filter(|r| r.enabled()) else {
        return reply(StatusCode::NOT_FOUND, "The file relay is off");
    };
    let Some((_, room)) = ctx.session(&headers, &q) else {
        return reply(StatusCode::UNAUTHORIZED, "Unknown session token");
    };
    let mode = match q.mode.as_deref() {
        Some("download") => Mode::Download,
        _ => Mode::Stream,
    };
    crate::relay::http::get_file(relay, &room, &id, &method, &headers, mode).await
}

async fn upload(
    State(ctx): State<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<Q>,
    headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    let Some(relay) = ctx.ext.relay.as_ref().filter(|r| r.enabled()) else {
        return reply(StatusCode::NOT_FOUND, "The file relay is off");
    };
    let Some((conn, _)) = ctx.session(&headers, &q) else {
        return reply(StatusCode::UNAUTHORIZED, "Unknown session token");
    };
    crate::relay::http::put_upload(relay, conn, &id, body).await
}

async fn proxy(
    State(ctx): State<Ctx>,
    ConnectInfo(meta): ConnectInfo<ConnMeta>,
    req: Request,
) -> Response<Body> {
    match &ctx.ext.proxy {
        Some(p) if p.target().is_some() => p.forward(req, meta.peer.ip(), meta.tls).await,
        _ => reply(StatusCode::NOT_FOUND, "Not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_byte_decides() {
        assert_eq!(sniff(b'{'), Sniff::Syncplay);
        assert_eq!(sniff(b' '), Sniff::Syncplay);
        assert_eq!(sniff(0x16), Sniff::Tls);
        assert_eq!(sniff(b'G'), Sniff::Http);
        assert_eq!(sniff(b'P'), Sniff::Http);
    }
}
