//! One port for everything: the first byte a client sends on the Syncplay
//! port says what it speaks. `{` is Syncplay (JSON lines, maybe STARTTLS),
//! `0x16` is a TLS handshake for HTTPS, and an uppercase ASCII letter starts
//! an HTTP request. HTTP connections are handed to an axum router through a
//! channel-backed [`Listener`]: the relay endpoints, a probe, the browser
//! client's WebSocket (`/yarmiplay/ws`, a normal Syncplay session with one
//! JSON line per text message) and MyAnimeList passthrough, and the shared
//! Jellyfin or Plex for every other path (see [`route`]). Browsers are only
//! let in from the allowed origins ([`cors`]). Vanilla mode never sniffs.

use super::room::{ConnId, ServerState};
use super::server::Extensions;
use crate::relay::scheduler::Mode;
use axum::body::Body;
use axum::extract::connect_info::Connected;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Response, StatusCode};
use axum::middleware::Next;
use axum::routing::{any, get, post, put};
use axum::serve::{IncomingStream, Listener};
use axum::Router;
use futures_util::future::BoxFuture;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
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

/// Runs a Syncplay session over a stream (the WebSocket bridge's end):
/// `(io, peer, tls)`. Set by the server once it exists.
pub type SessionFn = Arc<dyn Fn(BoxIo, SocketAddr, bool) -> BoxFuture<'static, ()> + Send + Sync>;
pub type SessionSlot = Arc<OnceLock<SessionFn>>;

/// Where the MyAnimeList passthrough sends requests: MyAnimeList itself,
/// or a stand-in in tests.
#[derive(Debug, Clone)]
pub struct MalUpstream {
    pub token_url: String,
    pub api_base: String,
}

impl Default for MalUpstream {
    fn default() -> Self {
        Self {
            token_url: "https://myanimelist.net/v1/oauth2/token".into(),
            api_base: "https://api.myanimelist.net/v2".into(),
        }
    }
}

/// Requests per minute from one address to the MyAnimeList passthrough.
const MAL_API_PER_MINUTE: usize = 60;
const MAL_TOKEN_PER_MINUTE: usize = 10;
/// Upper bound on a passthrough request or answer.
const MAL_BODY_LIMIT: usize = 4 << 20;

struct MalPass {
    upstream: MalUpstream,
    http: reqwest::Client,
    hits: Mutex<HashMap<(IpAddr, bool), VecDeque<Instant>>>,
}

impl MalPass {
    /// Count a request from `ip`; false once it's over the minute's limit.
    fn allow(&self, ip: IpAddr, token: bool) -> bool {
        let limit = if token {
            MAL_TOKEN_PER_MINUTE
        } else {
            MAL_API_PER_MINUTE
        };
        let now = Instant::now();
        let mut hits = self.hits.lock();
        hits.retain(|_, q| {
            q.back()
                .is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60))
        });
        let q = hits.entry((ip, token)).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            q.pop_front();
        }
        if q.len() >= limit {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[derive(Clone)]
struct Ctx {
    state: Weak<Mutex<ServerState>>,
    ext: Extensions,
    sessions: SessionSlot,
    mal: Arc<MalPass>,
}

impl Ctx {
    fn vanilla(&self) -> bool {
        self.state.upgrade().is_none_or(|s| s.lock().vanilla())
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.state
            .upgrade()
            .is_some_and(|s| s.lock().origin_allowed(origin))
    }

    /// The session behind `Authorization: Bearer <token>`, `X-Yarmiplay-Session: <token>`
    /// (for browsers, whose `Authorization` may carry something else) or `?t=<token>`.
    fn session(&self, headers: &HeaderMap, q: &Q) -> Option<(ConnId, String)> {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim);
        let custom = headers
            .get("x-yarmiplay-session")
            .and_then(|v| v.to_str().ok())
            .map(str::trim);
        let token = bearer.or(custom).or(q.t.as_deref())?;
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
/// WebSocket sessions start through `sessions` once the server fills it in.
pub fn start_http(
    state: Weak<Mutex<ServerState>>,
    ext: Extensions,
    local: SocketAddr,
    sessions: SessionSlot,
) -> (HttpSender, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(64);
    let http = crate::net::client_builder()
        .user_agent(concat!("YarmiplayServerTV/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let mal = Arc::new(MalPass {
        upstream: ext.mal_upstream.clone().unwrap_or_default(),
        http,
        hits: Mutex::new(HashMap::new()),
    });
    let ctx = Ctx {
        state,
        ext,
        sessions,
        mal,
    };
    let app = Router::new()
        .route("/yarmiplay/info", get(info))
        .route("/yarmiplay/ws", get(ws))
        .route("/yarmiplay/files/{id}", get(file))
        .route("/yarmiplay/upload/{id}", put(upload))
        .route("/yarmiplay/mal/token", post(mal_token))
        .route("/yarmiplay/mal/api/{*path}", any(mal_api))
        .fallback(proxy)
        .layer(axum::middleware::from_fn_with_state(ctx.clone(), cors))
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

const CORS_METHODS: &str = "GET, HEAD, PUT, POST, PATCH, DELETE, OPTIONS";
const CORS_HEADERS: &str =
    "Authorization, Range, Content-Type, X-Yarmiplay-Session, X-MAL-CLIENT-ID";
const CORS_EXPOSE: &str = "Content-Range, Accept-Ranges, Content-Length, Content-Type";

/// A page served from this very address (e.g. Jellyfin's web app through the
/// port) is the same origin, not a cross-origin browser client.
fn same_origin(origin: &str, headers: &HeaderMap) -> bool {
    let authority = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or("");
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| !authority.is_empty() && h.eq_ignore_ascii_case(authority))
}

/// Browsers: only the allowed origins get in (403 otherwise), preflights are
/// answered here (with Chrome's private-network opt-in), and answers carry
/// the CORS headers, replacing any from Jellyfin or Plex. Requests without an
/// `Origin` (native clients) and same-origin ones pass untouched.
async fn cors(State(ctx): State<Ctx>, req: Request, next: Next) -> Response<Body> {
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return next.run(req).await;
    };
    if same_origin(&origin, req.headers()) {
        return next.run(req).await;
    }
    if !ctx.origin_allowed(&origin) {
        return reply(
            StatusCode::FORBIDDEN,
            "This web page isn't allowed to use this server",
        );
    }
    let Ok(allow) = HeaderValue::from_str(&origin) else {
        return reply(StatusCode::FORBIDDEN, "Bad Origin");
    };
    if req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NO_CONTENT;
        let h = r.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow);
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(CORS_METHODS),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(CORS_HEADERS),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        if req
            .headers()
            .get("access-control-request-private-network")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"true"))
        {
            h.insert(
                "access-control-allow-private-network",
                HeaderValue::from_static("true"),
            );
        }
        return r;
    }
    let mut r = next.run(req).await;
    let h = r.headers_mut();
    let upstream: Vec<header::HeaderName> = h
        .keys()
        .filter(|k| k.as_str().starts_with("access-control-"))
        .cloned()
        .collect();
    for k in upstream {
        h.remove(k);
    }
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow);
    h.append(header::VARY, HeaderValue::from_static("Origin"));
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(CORS_EXPOSE),
    );
    h.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("cross-origin"),
    );
    r
}

/// `GET /yarmiplay/ws`: a Syncplay session over a WebSocket, for the browser
/// client. Each text message is one JSON line, both ways.
async fn ws(
    State(ctx): State<Ctx>,
    ConnectInfo(meta): ConnectInfo<ConnMeta>,
    upgrade: WebSocketUpgrade,
) -> Response<Body> {
    let Some(start) = ctx.sessions.get().cloned() else {
        return reply(StatusCode::SERVICE_UNAVAILABLE, "The server is starting");
    };
    upgrade
        .max_message_size(super::protocol::MAX_LINE_LENGTH + 2)
        .on_upgrade(move |socket| bridge(socket, start, meta))
}

/// Carry a WebSocket to a Syncplay session over an in-memory pipe: text
/// messages become lines (`\n` added), lines become text messages (line
/// ending dropped). Binary frames are ignored, pings answered, and the socket
/// closes when the session ends.
async fn bridge(socket: WebSocket, start: SessionFn, meta: ConnMeta) {
    let (ours, theirs) = tokio::io::duplex(256 * 1024);
    let session = tokio::spawn(start(Box::new(theirs), meta.peer, meta.tls));
    let (sink, mut stream) = socket.split();
    let (rd, mut wr) = tokio::io::split(ours);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

    let writer = tokio::spawn(async move {
        let mut sink = sink;
        while let Some(m) = out_rx.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let pongs = out_tx.clone();
    let inbound = tokio::spawn(async move {
        while let Some(Ok(m)) = stream.next().await {
            match m {
                Message::Text(t) => {
                    let line = t.as_str().trim_end_matches(['\r', '\n']);
                    if wr.write_all(line.as_bytes()).await.is_err()
                        || wr.write_all(b"\n").await.is_err()
                    {
                        break;
                    }
                }
                Message::Ping(p) => {
                    let _ = pongs.send(Message::Pong(p));
                }
                Message::Close(_) => break,
                Message::Binary(_) | Message::Pong(_) => {}
            }
        }
        let _ = wr.shutdown().await;
    });

    let mut lines = BufReader::new(rd).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        let l = l.trim_end_matches('\r');
        if !l.is_empty() && out_tx.send(Message::Text(l.to_string().into())).is_err() {
            break;
        }
    }
    let _ = out_tx.send(Message::Close(None));
    drop(out_tx);
    inbound.abort();
    let _ = writer.await;
    let _ = session.await;
}

/// The MyAnimeList passthrough needs an allowed browser origin (MyAnimeList
/// itself turns browsers away) and stays under a per-address rate limit.
fn mal_check(
    ctx: &Ctx,
    headers: &HeaderMap,
    meta: &ConnMeta,
    token: bool,
) -> Option<Response<Body>> {
    let allowed = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|o| ctx.origin_allowed(o));
    if !allowed {
        return Some(reply(
            StatusCode::FORBIDDEN,
            "Only the YarmiplayTV web app may use this",
        ));
    }
    if !ctx.mal.allow(meta.peer.ip(), token) {
        return Some(reply(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many MyAnimeList requests; try again in a minute",
        ));
    }
    None
}

/// MyAnimeList's answer, as is (bodies and tokens are never logged).
async fn mal_reply(res: Result<reqwest::Response, reqwest::Error>) -> Response<Body> {
    let res = match res {
        Ok(r) => r,
        Err(e) => {
            return reply(
                StatusCode::BAD_GATEWAY,
                &format!("MyAnimeList didn't answer: {}", e.without_url()),
            )
        }
    };
    let status = StatusCode::from_u16(res.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = res.headers().get(reqwest::header::CONTENT_TYPE).cloned();
    let body = match res.bytes().await {
        Ok(b) if b.len() <= MAL_BODY_LIMIT => b,
        Ok(_) => return reply(StatusCode::BAD_GATEWAY, "MyAnimeList's answer is too large"),
        Err(e) => {
            return reply(
                StatusCode::BAD_GATEWAY,
                &format!("MyAnimeList's answer: {}", e.without_url()),
            )
        }
    };
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    if let Some(ct) = content_type.and_then(|c| HeaderValue::from_bytes(c.as_bytes()).ok()) {
        r.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    r
}

/// `POST /yarmiplay/mal/token`: MyAnimeList's OAuth token endpoint.
async fn mal_token(
    State(ctx): State<Ctx>,
    ConnectInfo(meta): ConnectInfo<ConnMeta>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response<Body> {
    if let Some(r) = mal_check(&ctx, &headers, &meta, true) {
        return r;
    }
    let res = ctx
        .mal
        .http
        .post(&ctx.mal.upstream.token_url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await;
    mal_reply(res).await
}

/// `ANY /yarmiplay/mal/api/{*path}`: MyAnimeList's API v2, with the method,
/// query and body as sent, and the caller's bearer token or Client ID.
async fn mal_api(
    State(ctx): State<Ctx>,
    ConnectInfo(meta): ConnectInfo<ConnMeta>,
    Path(path): Path<String>,
    method: Method,
    headers: HeaderMap,
    req: Request,
) -> Response<Body> {
    if let Some(r) = mal_check(&ctx, &headers, &meta, false) {
        return r;
    }
    if path.split('/').any(|p| p == ".." || p.contains('\\')) {
        return reply(StatusCode::BAD_REQUEST, "Bad path");
    }
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let url = format!(
        "{}/{}{query}",
        ctx.mal.upstream.api_base.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let Ok(method) = reqwest::Method::from_bytes(method.as_str().as_bytes()) else {
        return reply(StatusCode::METHOD_NOT_ALLOWED, "Bad method");
    };
    let body = match axum::body::to_bytes(req.into_body(), MAL_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return reply(StatusCode::PAYLOAD_TOO_LARGE, "Request too large"),
    };
    let mut out = ctx.mal.http.request(method, url);
    for name in [
        header::AUTHORIZATION.as_str(),
        "x-mal-client-id",
        header::CONTENT_TYPE.as_str(),
    ] {
        if let Some(v) = headers.get(name) {
            out = out.header(name, v.as_bytes());
        }
    }
    if !body.is_empty() {
        out = out.body(body);
    }
    mal_reply(out.send().await).await
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

/// Which shared media server a proxied request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Jellyfin,
    Plex,
}

/// Paths only Plex serves. Jellyfin's own routes are capitalized
/// (`/Library/...`), so these are compared case-sensitively.
const PLEX_PATHS: [&str; 7] = [
    "/identity",
    "/library",
    "/:",
    "/photo",
    "/hubs",
    "/video",
    "/media",
];

/// Plex clients mark every request with `X-Plex-*` headers or an
/// `X-Plex-Token` query parameter; Jellyfin clients never do.
fn looks_like_plex(headers: &HeaderMap, path: &str, query: &str) -> bool {
    headers.keys().any(|k| k.as_str().starts_with("x-plex-"))
        || query.split('&').any(|p| {
            p.split('=')
                .next()
                .is_some_and(|k| k.eq_ignore_ascii_case("X-Plex-Token"))
        })
        || PLEX_PATHS.iter().any(|p| {
            path.strip_prefix(p)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
}

/// With one server shared, everything goes to it; with both, Plex's marks
/// decide.
fn route(
    jellyfin: bool,
    plex: bool,
    headers: &HeaderMap,
    path: &str,
    query: &str,
) -> Option<Target> {
    match (jellyfin, plex) {
        (false, false) => None,
        (true, false) => Some(Target::Jellyfin),
        (false, true) => Some(Target::Plex),
        (true, true) if looks_like_plex(headers, path, query) => Some(Target::Plex),
        (true, true) => Some(Target::Jellyfin),
    }
}

async fn proxy(
    State(ctx): State<Ctx>,
    ConnectInfo(meta): ConnectInfo<ConnMeta>,
    req: Request,
) -> Response<Body> {
    let active = |p: &Option<std::sync::Arc<crate::net::proxy::LocalProxy>>| {
        p.as_ref().filter(|p| p.target().is_some()).cloned()
    };
    let (jellyfin, plex) = (active(&ctx.ext.proxy), active(&ctx.ext.plex_proxy));
    let target = route(
        jellyfin.is_some(),
        plex.is_some(),
        req.headers(),
        req.uri().path(),
        req.uri().query().unwrap_or(""),
    );
    let proxy = match target {
        Some(Target::Jellyfin) => jellyfin,
        Some(Target::Plex) => plex,
        None => None,
    };
    match proxy {
        Some(p) => p.forward(req, meta.peer.ip(), meta.tls).await,
        None => reply(StatusCode::NOT_FOUND, "Not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syncplay::protocol::line;
    use crate::syncplay::{SyncplayOptions, SyncplayServer};
    use crate::tls::CertStore;
    use serde_json::{json, Value};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    const ALLOWED: &str = "http://localhost:5173";

    async fn start(opts: SyncplayOptions, tls: CertStore, ext: Extensions) -> SyncplayServer {
        crate::net::install_crypto_provider();
        SyncplayServer::start(0, opts, tls, Arc::new(|| {}), ext)
            .await
            .unwrap()
    }

    fn hello(name: &str) -> String {
        json!({ "Hello": { "username": name, "room": { "name": "lobby" }, "version": "1.2.255", "realversion": "1.7.4",
            "features": { "yarmiplay": { "protocol": 1 } } } })
        .to_string()
    }

    /// The next text message that is a JSON object with `key`.
    async fn until<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>, key: &str) -> Value
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .expect("timed out")
                .expect("closed")
                .unwrap();
            if let WsMessage::Text(t) = m {
                assert!(
                    !t.ends_with('\n'),
                    "one JSON line per message, without its line ending"
                );
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get(key).is_some() {
                    return v;
                }
            }
        }
    }

    async fn hello_and_list<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        // The browser never asks for STARTTLS; if a client does, it's told no and goes on.
        ws.send(WsMessage::Text(
            json!({ "TLS": { "startTLS": "send" } }).to_string().into(),
        ))
        .await
        .unwrap();
        assert_eq!(until(ws, "TLS").await["TLS"]["startTLS"], "false");
        ws.send(WsMessage::Text(hello("web").into())).await.unwrap();
        let h = until(ws, "Hello").await;
        assert_eq!(h["Hello"]["username"], "web");
        let caps = &h["Hello"]["features"]["yarmiplay"]["capabilities"];
        assert_eq!(caps["websocket"], true);
        assert_eq!(caps["mal"], true);
        assert_eq!(caps["roomSettings"], true);
        let session = until(ws, "Yarmiplay").await;
        assert!(session["Yarmiplay"]["session"]["token"].is_string());
        // Pings are answered.
        ws.send(WsMessage::Ping(vec![1, 2, 3].into()))
            .await
            .unwrap();
        ws.send(WsMessage::Text(line(&json!({ "List": null })).into()))
            .await
            .unwrap();
        let list = until(ws, "List").await;
        assert!(list["List"]["lobby"]["web"].is_object());
    }

    #[tokio::test]
    async fn websocket_hello_and_list_over_ws() {
        let server = start(
            SyncplayOptions::default(),
            Arc::default(),
            Extensions::default(),
        )
        .await;
        let tcp = TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let mut req = format!("ws://127.0.0.1:{}/yarmiplay/ws", server.port)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("origin", HeaderValue::from_static(ALLOWED));
        let (mut ws, resp) = tokio_tungstenite::client_async(req, tcp).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
        hello_and_list(&mut ws).await;
        assert_eq!(server.user_count(), 1);
        // Closing the socket ends the session.
        ws.close(None).await.unwrap();
        for _ in 0..50 {
            if server.user_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(server.user_count(), 0);
        server.stop();
    }

    #[tokio::test]
    async fn websocket_hello_and_list_over_wss() {
        crate::net::install_crypto_provider();
        let params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let config = crate::tls::server_config(&cert.pem(), &key.serialize_pem()).unwrap();
        let store: CertStore = Arc::new(parking_lot::RwLock::new(Some(Arc::new(
            crate::tls::CertBundle {
                host: "localhost".into(),
                names: vec!["localhost".into(), "home-lan.duckdns.org".into()],
                cert_pem: cert.pem(),
                key_pem: key.serialize_pem(),
                not_before: 0,
                not_after: i64::MAX,
                staging: true,
                fingerprint: String::new(),
                config,
            },
        ))));
        let server = start(SyncplayOptions::default(), store, Extensions::default()).await;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tcp = TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let tls = tokio_rustls::TlsConnector::from(Arc::new(client))
            .connect(
                rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                tcp,
            )
            .await
            .expect("TLS handshake");
        let mut req = format!("wss://localhost:{}/yarmiplay/ws", server.port)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "origin",
            HeaderValue::from_static("https://tv.yarmiplay.com"),
        );
        let (mut ws, _) = tokio_tungstenite::client_async(req, tls).await.unwrap();
        hello_and_list(&mut ws).await;
        // The LAN name on the certificate is advertised for browsers.
        let info: Value = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_der(cert.der()).unwrap())
            .resolve("localhost", SocketAddr::from(([127, 0, 0, 1], server.port)))
            .build()
            .unwrap()
            .get(format!("https://localhost:{}/yarmiplay/info", server.port))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        // Without logging in, only the certificate's name: the LAN name and addresses are for admitted clients.
        assert_eq!(info["tls"], serde_json::json!({ "name": "localhost" }));
        server.stop();
    }

    #[tokio::test]
    async fn other_origins_are_turned_away() {
        let server = start(
            SyncplayOptions::default(),
            Arc::default(),
            Extensions::default(),
        )
        .await;
        let http = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", server.port);
        let r = http
            .get(format!("{base}/yarmiplay/info"))
            .header("origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        // The WebSocket too.
        let tcp = TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let mut req = format!("ws://127.0.0.1:{}/yarmiplay/ws", server.port)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("origin", HeaderValue::from_static("https://evil.example"));
        assert!(tokio_tungstenite::client_async(req, tcp).await.is_err());
        // Native clients send no Origin; a page served from this address is same-origin.
        assert_eq!(
            http.get(format!("{base}/yarmiplay/info"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let same = format!("http://127.0.0.1:{}", server.port);
        assert_eq!(
            http.get(format!("{base}/yarmiplay/info"))
                .header("origin", same)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        server.stop();
    }

    #[tokio::test]
    async fn preflights_and_answers_carry_cors() {
        let server = start(
            SyncplayOptions::default(),
            Arc::default(),
            Extensions::default(),
        )
        .await;
        let http = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", server.port);
        let pre = http
            .request(
                reqwest::Method::OPTIONS,
                format!("{base}/yarmiplay/files/abc"),
            )
            .header("origin", ALLOWED)
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "range, authorization")
            .header("access-control-request-private-network", "true")
            .send()
            .await
            .unwrap();
        assert_eq!(pre.status(), 204);
        let h = pre.headers();
        assert_eq!(h["access-control-allow-origin"], ALLOWED);
        assert_eq!(h["vary"], "Origin");
        assert!(h["access-control-allow-methods"]
            .to_str()
            .unwrap()
            .contains("PUT"));
        assert!(h["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("Range"));
        assert_eq!(h["access-control-max-age"], "600");
        assert_eq!(h["access-control-allow-private-network"], "true");

        let info = http
            .get(format!("{base}/yarmiplay/info"))
            .header("origin", ALLOWED)
            .send()
            .await
            .unwrap();
        let h = info.headers();
        assert_eq!(h["access-control-allow-origin"], ALLOWED);
        assert_eq!(h["cross-origin-resource-policy"], "cross-origin");
        assert!(h["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .contains("Content-Range"));
        let v: Value = info.json().await.unwrap();
        assert_eq!(v["capabilities"]["websocket"], true);
        assert_eq!(v["capabilities"]["mal"], true);
        server.stop();
    }

    /// Stand-in for MyAnimeList: echoes what it was sent.
    async fn fake_mal() -> MalUpstream {
        async fn echo(method: Method, headers: HeaderMap, req: Request) -> axum::Json<Value> {
            let uri = req.uri().to_string();
            let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                .await
                .unwrap();
            let h = |n: &str| {
                headers
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            axum::Json(json!({
                "method": method.as_str(),
                "uri": uri,
                "authorization": h("authorization"),
                "clientId": h("x-mal-client-id"),
                "body": String::from_utf8_lossy(&body),
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(echo))
                .await
                .unwrap();
        });
        MalUpstream {
            token_url: format!("http://{addr}/v1/oauth2/token"),
            api_base: format!("http://{addr}/v2"),
        }
    }

    #[tokio::test]
    async fn myanimelist_passthrough() {
        let ext = Extensions {
            mal_upstream: Some(fake_mal().await),
            ..Extensions::default()
        };
        let server = start(SyncplayOptions::default(), Arc::default(), ext).await;
        let http = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}/yarmiplay/mal", server.port);

        // No session needed: signed-out lookups with the Client ID.
        let r = http
            .get(format!("{base}/api/anime/52991?fields=title"))
            .header("origin", ALLOWED)
            .header("x-mal-client-id", "abc123")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["access-control-allow-origin"], ALLOWED);
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["method"], "GET");
        assert_eq!(v["uri"], "/v2/anime/52991?fields=title");
        assert_eq!(v["clientId"], "abc123");

        // Signed in: the bearer token, method and body go through.
        let v: Value = http
            .patch(format!("{base}/api/anime/52991/my_list_status"))
            .header("origin", ALLOWED)
            .header("authorization", "Bearer tok")
            .header("content-type", "application/x-www-form-urlencoded")
            .body("num_watched_episodes=3")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["method"], "PATCH");
        assert_eq!(v["authorization"], "Bearer tok");
        assert_eq!(v["body"], "num_watched_episodes=3");

        let v: Value = http
            .post(format!("{base}/token"))
            .header("origin", ALLOWED)
            .body("grant_type=authorization_code&code=x")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["uri"], "/v1/oauth2/token");
        assert_eq!(v["body"], "grant_type=authorization_code&code=x");

        // Only the allowed web app, and not too often.
        let r = http
            .get(format!("{base}/api/anime/1"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let mut last = 0;
        for _ in 0..MAL_TOKEN_PER_MINUTE {
            last = http
                .post(format!("{base}/token"))
                .header("origin", ALLOWED)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16();
        }
        assert_eq!(last, 429, "the token endpoint has a tighter limit");
        server.stop();
    }

    #[tokio::test]
    async fn vanilla_mode_closes_all_of_it() {
        let server = start(
            SyncplayOptions::default(),
            Arc::default(),
            Extensions::default(),
        )
        .await;
        let http = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{}", server.port);
        assert_eq!(
            http.get(format!("{base}/yarmiplay/info"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        server.set_options(SyncplayOptions {
            vanilla_mode: true,
            ..SyncplayOptions::default()
        });
        // A connection kept alive from before gets 404 for every route.
        for (method, path) in [
            (reqwest::Method::GET, "/yarmiplay/ws"),
            (reqwest::Method::POST, "/yarmiplay/mal/token"),
            (reqwest::Method::GET, "/yarmiplay/mal/api/anime/1"),
            (reqwest::Method::OPTIONS, "/yarmiplay/files/x"),
        ] {
            let r = http
                .request(method, format!("{base}{path}"))
                .header("origin", ALLOWED)
                .header("access-control-request-method", "GET")
                .send()
                .await;
            if let Ok(r) = r {
                assert_eq!(r.status(), 404, "{path}");
            }
        }
        // A new connection isn't even read as HTTP.
        let tcp = TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let req = format!("ws://127.0.0.1:{}/yarmiplay/ws", server.port)
            .into_client_request()
            .unwrap();
        assert!(tokio_tungstenite::client_async(req, tcp).await.is_err());
        server.stop();
    }

    #[test]
    fn first_byte_decides() {
        assert_eq!(sniff(b'{'), Sniff::Syncplay);
        assert_eq!(sniff(b' '), Sniff::Syncplay);
        assert_eq!(sniff(0x16), Sniff::Tls);
        assert_eq!(sniff(b'G'), Sniff::Http);
        assert_eq!(sniff(b'P'), Sniff::Http);
    }

    #[test]
    fn requests_are_routed_to_the_right_media_server() {
        let none = HeaderMap::new();
        let mut plex_headers = HeaderMap::new();
        plex_headers.insert("x-plex-client-identifier", HeaderValue::from_static("abc"));
        let both = |h: &HeaderMap, path: &str, query: &str| route(true, true, h, path, query);

        assert_eq!(route(false, false, &none, "/identity", ""), None);
        assert_eq!(
            route(true, false, &none, "/identity", ""),
            Some(Target::Jellyfin),
            "with only Jellyfin shared, everything is Jellyfin's"
        );
        assert_eq!(
            route(false, true, &none, "/System/Info/Public", ""),
            Some(Target::Plex)
        );

        assert_eq!(both(&none, "/identity", ""), Some(Target::Plex));
        assert_eq!(both(&none, "/library/sections", ""), Some(Target::Plex));
        assert_eq!(
            both(&none, "/library/parts/1/2/file.mkv", ""),
            Some(Target::Plex)
        );
        assert_eq!(both(&none, "/:/timeline", ""), Some(Target::Plex));
        assert_eq!(
            both(&plex_headers, "/web/index.html", ""),
            Some(Target::Plex)
        );
        assert_eq!(both(&none, "/x", "a=1&X-Plex-Token=t"), Some(Target::Plex));
        assert_eq!(both(&none, "/x", "x-plex-token=t"), Some(Target::Plex));

        assert_eq!(
            both(&none, "/Library/VirtualFolders", ""),
            Some(Target::Jellyfin)
        );
        assert_eq!(
            both(&none, "/System/Info/Public", ""),
            Some(Target::Jellyfin)
        );
        assert_eq!(both(&none, "/web/index.html", ""), Some(Target::Jellyfin));
        assert_eq!(both(&none, "/identityx", ""), Some(Target::Jellyfin));
        assert_eq!(both(&none, "/libraryx/a", ""), Some(Target::Jellyfin));
        assert_eq!(
            both(&none, "/Items", "api_key=X-Plex"),
            Some(Target::Jellyfin)
        );
    }
}
