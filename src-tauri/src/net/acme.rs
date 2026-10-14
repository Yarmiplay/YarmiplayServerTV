//! Let's Encrypt certificate for `<name>.duckdns.org`, validated by DNS-01
//! through the DuckDNS TXT API, so no inbound port is needed for validation;
//! or for a domain of the user's own, validated by HTTP-01 on port 80 (the
//! domain must point at this PC and port 80 reach it).
//! Ported from GoogleSnakeOnline's `acme.rs` (DuckDNS route only).
//!
//! DuckDNS keeps a single TXT record per domain, so only one certificate order
//! for a given name may run at a time (across all apps using that name).

use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, Order, OrderStatus, RetryPolicy,
};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

pub const RENEW_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(30 * 60);
const MIN_CHECK_DELAY: Duration = Duration::from_secs(60);
const ORDER_TIMEOUT: Duration = Duration::from_secs(120);
const CERT_FILE: &str = "fullchain.pem";
const KEY_FILE: &str = "privkey.pem";
const ACCOUNT_FILE: &str = "account.json";
pub const DUCKDNS_API: &str = "https://www.duckdns.org";
const DUCKDNS_SUFFIX: &str = ".duckdns.org";
pub const DUCKDNS_IP_REFRESH: Duration = Duration::from_secs(5 * 60);
const DUCKDNS_ATTEMPTS: u32 = 4;
#[cfg(not(test))]
const DUCKDNS_RETRY_DELAY: Duration = Duration::from_secs(2);
#[cfg(test)]
const DUCKDNS_RETRY_DELAY: Duration = Duration::from_millis(10);
const DOH_URLS: [&str; 2] = [
    "https://dns.google/resolve",
    "https://cloudflare-dns.com/dns-query",
];
const TXT_PROPAGATION_TIMEOUT: Duration = Duration::from_secs(120);
const TXT_POLL_INTERVAL: Duration = Duration::from_secs(4);
/// Extra settle time after the TXT is visible, for Let's Encrypt's other vantage points.
const TXT_SETTLE: Duration = Duration::from_secs(10);
/// The port Let's Encrypt connects to for HTTP-01.
pub const HTTP01_PORT: u16 = 80;
const HTTP01_PREFIX: &str = "/.well-known/acme-challenge/";

/// DuckDNS subdomain + account token (the token is never logged or formatted).
#[derive(Clone, PartialEq, Eq)]
pub struct DuckDns {
    /// Label only, e.g. `yarmiplay` for `yarmiplay.duckdns.org`.
    pub subdomain: String,
    pub token: String,
}

impl std::fmt::Debug for DuckDns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDns")
            .field("subdomain", &self.subdomain)
            .field("token", &"<redacted>")
            .finish()
    }
}

impl DuckDns {
    /// Accepts `yarmiplay` or `yarmiplay.duckdns.org`.
    pub fn new(domain: &str, token: &str) -> Result<Self, String> {
        let subdomain = normalize_domain(domain)?;
        let token = token.trim();
        if token.is_empty() {
            return Err("DuckDNS token is empty".into());
        }
        Ok(Self {
            subdomain,
            token: token.to_string(),
        })
    }

    pub fn fqdn(&self) -> String {
        format!("{}{DUCKDNS_SUFFIX}", self.subdomain)
    }
}

/// `yarmiplay` / `YarmiPlay.duckdns.org.` -> `yarmiplay`.
pub fn normalize_domain(domain: &str) -> Result<String, String> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let subdomain = domain
        .strip_suffix(DUCKDNS_SUFFIX)
        .unwrap_or(&domain)
        .to_string();
    let valid = !subdomain.is_empty()
        && subdomain.len() <= 63
        && !subdomain.starts_with('-')
        && !subdomain.ends_with('-')
        && subdomain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid {
        return Err(format!(
            "invalid DuckDNS domain {domain:?} (expected e.g. myname or myname.duckdns.org)"
        ));
    }
    Ok(subdomain)
}

/// `TV.Example.com.` -> `tv.example.com`: a full domain name of the user's own.
pub fn normalize_own_domain(domain: &str) -> Result<String, String> {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let labels: Vec<&str> = d.split('.').collect();
    let tld_ok = labels.last().is_some_and(|t| t.chars().any(|c| c.is_ascii_alphabetic()));
    if labels.len() < 2 || d.len() > 253 || !labels.iter().all(|l| label_ok(l)) || !tld_ok {
        return Err(format!("invalid domain {domain:?} (expected e.g. tv.example.com)"));
    }
    Ok(d)
}

/// How Let's Encrypt is shown the server owns its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validation {
    /// DNS-01 through DuckDNS's TXT API.
    DuckDns {
        duck: DuckDns,
        /// A second DuckDNS name (same account) that points at the host's LAN
        /// address, so browsers at home can connect locally with a valid
        /// certificate. It goes on the same certificate.
        lan: Option<DuckDns>,
    },
    /// HTTP-01 on port 80 for a domain of the user's own (normalized).
    Http { domain: String },
}

impl Validation {
    /// The certificate's main name.
    pub fn host(&self) -> String {
        match self {
            Self::DuckDns { duck, .. } => duck.fqdn(),
            Self::Http { domain } => domain.clone(),
        }
    }

    pub fn duckdns(&self) -> Option<&DuckDns> {
        match self {
            Self::DuckDns { duck, .. } => Some(duck),
            Self::Http { .. } => None,
        }
    }

    pub fn lan(&self) -> Option<&DuckDns> {
        match self {
            Self::DuckDns { lan, .. } => lan.as_ref(),
            Self::Http { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AcmeOptions {
    pub dir: PathBuf,
    pub staging: bool,
    pub email: Option<String>,
    pub validation: Validation,
    /// DuckDNS API base URL override (tests); `None` = [`DUCKDNS_API`].
    pub duckdns_api: Option<String>,
    /// Custom ACME directory (e.g. Pebble) instead of Let's Encrypt.
    pub directory: Option<String>,
    /// HTTP-01 port override (tests); `None` = [`HTTP01_PORT`].
    pub http01_port: Option<u16>,
}

impl AcmeOptions {
    fn root(&self) -> PathBuf {
        if self.staging {
            self.dir.join("staging")
        } else {
            self.dir.clone()
        }
    }

    fn cert_dir(&self) -> PathBuf {
        self.root().join(self.validation.host())
    }

    pub fn duckdns_api(&self) -> &str {
        self.duckdns_api.as_deref().unwrap_or(DUCKDNS_API)
    }

    /// The names the certificate must cover: the main name, then the LAN name.
    pub fn names(&self) -> Vec<String> {
        let mut names = vec![self.validation.host()];
        names.extend(
            self.validation
                .lan()
                .map(DuckDns::fqdn)
                .filter(|n| n != &names[0]),
        );
        names
    }

    /// The DuckDNS name `host` belongs to.
    fn duck_for(&self, host: &str) -> Option<&DuckDns> {
        self.validation
            .duckdns()
            .into_iter()
            .chain(self.validation.lan())
            .find(|d| d.fqdn().eq_ignore_ascii_case(host))
    }

    fn directory_url(&self) -> String {
        match &self.directory {
            Some(url) => url.clone(),
            None if self.staging => LetsEncrypt::Staging.url().to_owned(),
            None => LetsEncrypt::Production.url().to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IssuedCert {
    pub host: String,
    /// Every DNS name the certificate covers (`host` first).
    pub names: Vec<String>,
    pub cert_pem: String,
    pub key_pem: String,
    /// Unix seconds.
    pub not_before: i64,
    pub not_after: i64,
}

impl IssuedCert {
    pub fn needs_renewal(&self, now: i64) -> bool {
        needs_renewal(self.not_before, self.not_after, now)
    }

    pub fn renew_at(&self) -> i64 {
        renew_at(self.not_before, self.not_after)
    }
}

/// Renew once half of the certificate lifetime has elapsed.
pub fn needs_renewal(not_before: i64, not_after: i64, now: i64) -> bool {
    if not_after <= not_before {
        return true;
    }
    now >= renew_at(not_before, not_after)
}

fn renew_at(not_before: i64, not_after: i64) -> i64 {
    not_before + (not_after - not_before) / 2
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    super::install_crypto_provider();
    crate::net::client_builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

/// DuckDNS `/update` with retries on network errors and 5xx. Errors never include the
/// request URL (it holds the token).
async fn duckdns_update(
    api: &str,
    duck: &DuckDns,
    extra: &[(&str, &str)],
) -> Result<String, String> {
    let mut delay = DUCKDNS_RETRY_DELAY;
    for attempt in 1..=DUCKDNS_ATTEMPTS {
        match duckdns_update_once(api, duck, extra).await {
            Ok(body) => return Ok(body),
            Err((e, true)) if attempt < DUCKDNS_ATTEMPTS => {
                warn!(error = %e, attempt, "DuckDNS update failed, retrying");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
            Err((e, _)) => return Err(e),
        }
    }
    unreachable!("DUCKDNS_ATTEMPTS is at least 1")
}

/// One `/update` call; the flag marks transient failures worth retrying.
async fn duckdns_update_once(
    api: &str,
    duck: &DuckDns,
    extra: &[(&str, &str)],
) -> Result<String, (String, bool)> {
    let client = http_client(Duration::from_secs(15)).map_err(|e| (e, false))?;
    let mut query: Vec<(&str, &str)> = vec![("domains", &duck.subdomain), ("token", &duck.token)];
    query.extend_from_slice(extra);
    query.push(("verbose", "true"));
    let res = client
        .get(format!("{}/update", api.trim_end_matches('/')))
        .query(&query)
        .send()
        .await
        .map_err(|e| (format!("DuckDNS request failed: {}", e.without_url()), true))?;
    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|e| (format!("DuckDNS response: {}", e.without_url()), true))?;
    if !status.is_success() {
        return Err((format!("DuckDNS HTTP {status}"), status.is_server_error()));
    }
    if body.trim_start().starts_with("OK") {
        Ok(body)
    } else {
        Err((
            format!(
                "DuckDNS rejected the update for {} (KO); check that the token belongs to the account that owns this domain",
                duck.fqdn()
            ),
            false,
        ))
    }
}

/// What DuckDNS has on record for the name.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DuckDnsRecord {
    pub ip: Option<IpAddr>,
    pub ipv6: Option<Ipv6Addr>,
}

/// Point `<subdomain>.duckdns.org` at `ip` (`None` = DuckDNS uses the caller's
/// IPv4) and, when given, `ipv6` (`None` leaves the AAAA record alone).
/// Returns what DuckDNS now has on record.
pub async fn duckdns_set_ip(
    api: &str,
    duck: &DuckDns,
    ip: Option<IpAddr>,
    ipv6: Option<Ipv6Addr>,
) -> Result<DuckDnsRecord, String> {
    let ip = ip.map(|ip| ip.to_string());
    let ipv6 = ipv6.map(|ip| ip.to_string());
    let mut extra: Vec<(&str, &str)> = Vec::new();
    if let Some(ip) = ip.as_deref() {
        extra.push(("ip", ip));
    }
    if let Some(ip) = ipv6.as_deref() {
        extra.push(("ipv6", ip));
    }
    let body = duckdns_update(api, duck, &extra).await?;
    if body.contains("UPDATED") {
        info!(domain = %duck.fqdn(), ip = ?ip, ipv6 = ?ipv6, "DuckDNS address updated");
    }
    // Verbose reply: OK, IPv4, IPv6, UPDATED|NOCHANGE.
    let line = |n| body.lines().nth(n).map(str::trim).unwrap_or("");
    Ok(DuckDnsRecord {
        ip: line(1).parse().ok(),
        ipv6: line(2).parse().ok(),
    })
}

pub async fn duckdns_set_txt(api: &str, duck: &DuckDns, value: &str) -> Result<(), String> {
    duckdns_update(api, duck, &[("txt", value)])
        .await
        .map(|_| ())
}

pub async fn duckdns_clear_txt(api: &str, duck: &DuckDns) -> Result<(), String> {
    duckdns_update(api, duck, &[("txt", "cleared"), ("clear", "true")])
        .await
        .map(|_| ())
}

/// TXT strings from a DNS-over-HTTPS JSON answer (quotes and chunking removed).
fn doh_txt_values(json: &serde_json::Value) -> Vec<String> {
    json.get("Answer")
        .and_then(|a| a.as_array())
        .map(|answers| {
            answers
                .iter()
                .filter(|a| a.get("type").and_then(|t| t.as_u64()) == Some(16))
                .filter_map(|a| a.get("data").and_then(|d| d.as_str()))
                .map(|d| {
                    d.split('"')
                        .enumerate()
                        .filter(|(i, _)| i % 2 == 1)
                        .map(|(_, s)| s)
                        .collect::<String>()
                })
                .map(|joined| joined.trim().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until public resolvers return `value` for `name`, so Let's Encrypt will too.
async fn wait_for_txt(name: &str, value: &str) -> Result<(), String> {
    let client = http_client(Duration::from_secs(10))?;
    let deadline = tokio::time::Instant::now() + TXT_PROPAGATION_TIMEOUT;
    loop {
        for url in DOH_URLS {
            let res = client
                .get(url)
                .query(&[("name", name), ("type", "TXT")])
                .header("accept", "application/dns-json")
                .send()
                .await;
            let Ok(res) = res else { continue };
            let Ok(json) = res.json::<serde_json::Value>().await else {
                continue;
            };
            if doh_txt_values(&json).iter().any(|v| v == value) {
                info!(%name, resolver = url, "ACME TXT record visible in public DNS");
                tokio::time::sleep(TXT_SETTLE).await;
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "TXT record for {name} did not appear in public DNS within {}s; DuckDNS may be slow or down, try again later",
                TXT_PROPAGATION_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(TXT_POLL_INTERVAL).await;
    }
}

/// The addresses public DNS has for `name` (A, then AAAA), for showing where
/// the user's own domain points.
pub async fn resolve_public(name: &str) -> Vec<IpAddr> {
    let Ok(client) = http_client(Duration::from_secs(10)) else { return Vec::new() };
    let mut out = Vec::new();
    for kind in ["A", "AAAA"] {
        for url in DOH_URLS {
            let res = client
                .get(url)
                .query(&[("name", name), ("type", kind)])
                .header("accept", "application/dns-json")
                .send()
                .await;
            let Ok(json) = (match res {
                Ok(r) => r.json::<serde_json::Value>().await,
                Err(e) => Err(e),
            }) else {
                continue;
            };
            out.extend(doh_addresses(&json));
            break;
        }
    }
    out
}

/// A and AAAA addresses from a DNS-over-HTTPS JSON answer (CNAMEs followed by the resolver).
fn doh_addresses(json: &serde_json::Value) -> Vec<IpAddr> {
    json.get("Answer")
        .and_then(|a| a.as_array())
        .map(|answers| {
            answers
                .iter()
                .filter(|a| matches!(a.get("type").and_then(|t| t.as_u64()), Some(1 | 28)))
                .filter_map(|a| a.get("data")?.as_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a PEM chain; the leaf must carry `expected` as a DNS SAN.
pub fn parse_cert(cert_pem: &str, key_pem: &str, expected: &str) -> Result<IssuedCert, String> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| format!("certificate PEM: {e}"))?;
    let cert = pem
        .parse_x509()
        .map_err(|e| format!("certificate DER: {e}"))?;
    let sans: Vec<String> = cert
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    x509_parser::extensions::GeneralName::DNSName(dns) => {
                        Some(dns.to_ascii_lowercase())
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    if !sans.iter().any(|n| n.eq_ignore_ascii_case(expected)) {
        return Err(format!("certificate does not cover {expected}"));
    }
    if !key_pem.contains("PRIVATE KEY") {
        return Err("private key PEM missing".into());
    }
    let mut names = vec![expected.to_ascii_lowercase()];
    names.extend(
        sans.into_iter()
            .filter(|n| !n.eq_ignore_ascii_case(expected)),
    );
    Ok(IssuedCert {
        names,
        host: expected.to_string(),
        cert_pem: cert_pem.to_string(),
        key_pem: key_pem.to_string(),
        not_before: cert.validity().not_before.timestamp(),
        not_after: cert.validity().not_after.timestamp(),
    })
}

pub fn load_cached(opts: &AcmeOptions) -> Option<IssuedCert> {
    let dir = opts.cert_dir();
    let cert = std::fs::read_to_string(dir.join(CERT_FILE)).ok()?;
    let key = std::fs::read_to_string(dir.join(KEY_FILE)).ok()?;
    match parse_cert(&cert, &key, &opts.validation.host()) {
        Ok(c) => Some(c),
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "cached certificate invalid");
            None
        }
    }
}

fn save_cert(opts: &AcmeOptions, cert_pem: &str, key_pem: &str) -> Result<(), String> {
    let dir = opts.cert_dir();
    write_atomic(&dir.join(KEY_FILE), key_pem)?;
    write_atomic(&dir.join(CERT_FILE), cert_pem)
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    crate::paths::write_atomic(path, contents.as_bytes())
}

async fn load_or_create_account(opts: &AcmeOptions) -> Result<Account, String> {
    let root = opts.root();
    let path = root.join(ACCOUNT_FILE);
    let builder = || Account::builder().map_err(|e| format!("ACME HTTP client: {e}"));

    if let Ok(raw) = std::fs::read_to_string(&path) {
        match serde_json::from_str::<AccountCredentials>(&raw) {
            Ok(creds) => {
                return builder()?
                    .from_credentials(creds)
                    .await
                    .map_err(|e| format!("restore ACME account {}: {e}", path.display()));
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "ACME account file unreadable, creating a new account")
            }
        }
    }

    let contact = opts
        .email
        .as_ref()
        .filter(|e| !e.trim().is_empty())
        .map(|e| format!("mailto:{}", e.trim()));
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let (account, creds) = builder()?
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            opts.directory_url(),
            None,
        )
        .await
        .map_err(|e| format!("create ACME account: {e}"))?;
    let json = serde_json::to_string_pretty(&creds).map_err(|e| e.to_string())?;
    write_atomic(&path, &json)?;
    info!(staging = opts.staging, "ACME account created");
    Ok(account)
}

/// A tiny HTTP server answering Let's Encrypt's HTTP-01 requests while an
/// order runs: `/.well-known/acme-challenge/<token>` -> key authorization.
pub struct Http01Server {
    tokens: Arc<parking_lot::Mutex<HashMap<String, String>>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Http01Server {
    /// Listen on `port` (IPv4 required, IPv6 when it can).
    pub async fn start(port: u16) -> Result<Self, String> {
        let tokens: Arc<parking_lot::Mutex<HashMap<String, String>>> = Arc::default();
        let v4 = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                return format!(
                    "Couldn't listen on port {port} for Let's Encrypt: this system only lets privileged programs use ports below 1024. On Linux, allow it with: sudo setcap cap_net_bind_service=+ep <path to the app>, or: sudo sysctl net.ipv4.ip_unprivileged_port_start=80"
                );
            }
            format!("Couldn't listen on port {port} for Let's Encrypt ({e}). Another program (a web server, IIS, Skype) may be using it: stop it, then renew")
        })?;
        let mut listeners = vec![v4];
        // Where IPv6 sockets are dual-stack, the IPv4 bind already covers it.
        if let Ok(v6) = tokio::net::TcpListener::bind(("::", port)).await {
            listeners.push(v6);
        }
        let tasks = listeners
            .into_iter()
            .map(|l| {
                let tokens = tokens.clone();
                tokio::spawn(async move {
                    while let Ok((stream, _)) = l.accept().await {
                        let tokens = tokens.clone();
                        tokio::spawn(async move {
                            let _ = tokio::time::timeout(
                                Duration::from_secs(10),
                                answer_http01(stream, &tokens),
                            )
                            .await;
                        });
                    }
                })
            })
            .collect();
        Ok(Self { tokens, tasks })
    }

    pub fn add(&self, token: String, key_authorization: String) {
        self.tokens.lock().insert(token, key_authorization);
    }
}

impl Drop for Http01Server {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// The key authorization for a request line's challenge path, if known.
fn http01_answer(request: &str, tokens: &HashMap<String, String>) -> Option<String> {
    let line = request.lines().next()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?, parts.next()?);
    if method != "GET" && method != "HEAD" {
        return None;
    }
    let token = path.strip_prefix(HTTP01_PREFIX)?;
    tokens.get(token).cloned()
}

async fn answer_http01(
    mut stream: tokio::net::TcpStream,
    tokens: &parking_lot::Mutex<HashMap<String, String>>,
) {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 8192 {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let request = String::from_utf8_lossy(&buf);
    let answer = http01_answer(&request, &tokens.lock());
    let reply = match &answer {
        Some(body) => {
            info!("answered Let's Encrypt's HTTP-01 request");
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
    };
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Issuance: DNS-01 for DuckDNS names (each with its own TXT record, always
/// cleared afterwards), or HTTP-01 on port 80 for the user's own domain.
async fn issue(opts: &AcmeOptions) -> Result<(String, String), String> {
    let account = load_or_create_account(opts).await?;
    let names = opts.names();
    let host = names[0].clone();
    let identifiers: Vec<Identifier> = names.iter().cloned().map(Identifier::Dns).collect();
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(|e| format!("new order for {}: {e}", names.join(", ")))?;
    info!(names = %names.join(", "), staging = opts.staging, "ACME order started");
    let http = match &opts.validation {
        Validation::Http { .. } => Some(Http01Server::start(opts.http01_port.unwrap_or(HTTP01_PORT)).await?),
        Validation::DuckDns { .. } => None,
    };
    let result = drive_order(&mut order, opts, &host, http.as_ref()).await;
    drop(http);
    for duck in opts.validation.duckdns().into_iter().chain(opts.validation.lan()) {
        if let Err(e) = duckdns_clear_txt(opts.duckdns_api(), duck).await {
            warn!(error = %e, domain = %duck.fqdn(), "clearing the DuckDNS TXT record failed");
        }
    }
    result
}

async fn drive_order(
    order: &mut Order,
    opts: &AcmeOptions,
    host: &str,
    http: Option<&Http01Server>,
) -> Result<(String, String), String> {
    {
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authz = result.map_err(|e| format!("fetch authorization: {e}"))?;
            match authz.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                other => return Err(format!("authorization for {host} is {other:?}")),
            }
            let name = match authz.identifier().identifier {
                Identifier::Dns(name) => name.clone(),
                other => return Err(format!("unexpected identifier {other:?}")),
            };
            if let Some(http) = http {
                let mut challenge = authz
                    .challenge(ChallengeType::Http01)
                    .ok_or_else(|| format!("no HTTP-01 challenge offered for {name}"))?;
                http.add(challenge.token.clone(), challenge.key_authorization().as_str().to_string());
                info!(%name, "answering Let's Encrypt on port 80");
                challenge
                    .set_ready()
                    .await
                    .map_err(|e| format!("challenge ready: {e}"))?;
                continue;
            }
            let duck = opts
                .duck_for(&name)
                .ok_or_else(|| format!("{name} isn't one of this server's DuckDNS names"))?
                .clone();
            let mut challenge = authz
                .challenge(ChallengeType::Dns01)
                .ok_or_else(|| format!("no DNS-01 challenge offered for {name}"))?;
            let value = challenge.key_authorization().dns_value();
            duckdns_set_txt(opts.duckdns_api(), &duck, &value).await?;
            info!(%name, "DuckDNS TXT record set");
            wait_for_txt(&format!("_acme-challenge.{name}"), &value).await?;
            challenge
                .set_ready()
                .await
                .map_err(|e| format!("challenge ready: {e}"))?;
        }
    }

    let policy = RetryPolicy::new().timeout(ORDER_TIMEOUT);
    let hint = match &opts.validation {
        Validation::Http { .. } => format!(
            "Let's Encrypt must reach http://{host}/ on port 80. Check that {host} points at your public IP (an A record, or a CNAME to your DuckDNS name), that port 80 is forwarded to this PC (switch on UPnP, or forward it on the router), and that your ISP doesn't block port 80."
        ),
        Validation::DuckDns { .. } => format!(
            "Let's Encrypt must see the TXT record _acme-challenge.{host} (and the LAN name's, if set) through DuckDNS. Check the DuckDNS token, that both names belong to that account, and that DuckDNS is up."
        ),
    };
    let status = order
        .poll_ready(&policy)
        .await
        .map_err(|e| format!("validation failed: {e}. {hint}"))?;
    if status != OrderStatus::Ready {
        let detail = challenge_errors(order).await;
        return Err(format!("validation failed ({status:?}){detail}. {hint}"));
    }

    let key_pem = order
        .finalize()
        .await
        .map_err(|e| format!("finalize: {e}"))?;
    let cert_pem = order
        .poll_certificate(&policy)
        .await
        .map_err(|e| format!("download certificate: {e}"))?;
    Ok((cert_pem, key_pem))
}

async fn challenge_errors(order: &mut Order) -> String {
    let mut details = Vec::new();
    let mut authorizations = order.authorizations();
    while let Some(Ok(mut authz)) = authorizations.next().await {
        if let Ok(state) = authz.refresh().await {
            details.extend(
                state
                    .challenges
                    .iter()
                    .filter_map(|c| c.error.as_ref().map(|p| p.to_string())),
            );
        }
    }
    if details.is_empty() {
        String::new()
    } else {
        format!(": {}", details.join("; "))
    }
}

/// A cached certificate does unless it's due for renewal or misses a name
/// (a LAN name added since the last order needs a new certificate).
fn cached_is_enough(c: &IssuedCert, names: &[String], now: i64) -> bool {
    !c.needs_renewal(now)
        && names
            .iter()
            .all(|n| c.names.iter().any(|x| x.eq_ignore_ascii_case(n)))
}

/// Reuse a cached certificate unless it is due for renewal (or `force`), else issue a new one.
/// A failed renewal keeps serving the cached certificate while it is still valid.
pub async fn load_or_issue(opts: &AcmeOptions, force: bool) -> Result<IssuedCert, String> {
    let host = opts.validation.host();
    let now = unix_now();
    let cached = load_cached(opts);
    if let Some(c) = &cached {
        if !force && cached_is_enough(c, &opts.names(), now) {
            info!(%host, not_after = c.not_after, "using cached certificate");
            return Ok(c.clone());
        }
    }
    match issue(opts).await {
        Ok((cert_pem, key_pem)) => {
            let issued = parse_cert(&cert_pem, &key_pem, &host)?;
            save_cert(opts, &cert_pem, &key_pem)?;
            info!(%host, not_after = issued.not_after, staging = opts.staging, "certificate issued");
            Ok(issued)
        }
        Err(e) => match cached {
            Some(c) if now < c.not_after => {
                warn!(%host, error = %e, "renewal failed, keeping the cached certificate");
                Ok(c)
            }
            _ => Err(e),
        },
    }
}

pub fn next_check_delay(cert: &IssuedCert, now: i64, failed: bool) -> Duration {
    if failed {
        return RETRY_AFTER_FAILURE;
    }
    let until_renew = cert.renew_at().saturating_sub(now).max(0) as u64;
    Duration::from_secs(until_renew).clamp(MIN_CHECK_DELAY, RENEW_CHECK_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::Arc;

    const DAY: i64 = 86_400;
    const TEST_TOKEN: &str = "0000aaaa-token-secret-1111";

    fn opts(dir: &Path) -> AcmeOptions {
        AcmeOptions {
            dir: dir.to_path_buf(),
            staging: true,
            email: None,
            validation: Validation::DuckDns {
                duck: DuckDns::new("yarmiplay", TEST_TOKEN).unwrap(),
                lan: None,
            },
            duckdns_api: None,
            directory: None,
            http01_port: None,
        }
    }

    fn set_lan(o: &mut AcmeOptions, lan: Option<DuckDns>) {
        if let Validation::DuckDns { lan: l, .. } = &mut o.validation {
            *l = lan;
        }
    }

    fn self_signed(
        host: &str,
        not_before: (i32, u8, u8),
        not_after: (i32, u8, u8),
    ) -> (String, String) {
        let mut params = rcgen::CertificateParams::new(vec![host.to_string()]).unwrap();
        params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
        params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn renewal_at_half_life() {
        let nb = 1_000_000;
        let na = nb + 90 * DAY;
        assert!(!needs_renewal(nb, na, nb + 45 * DAY - 1));
        assert!(needs_renewal(nb, na, nb + 45 * DAY));
        assert!(needs_renewal(na, nb, nb), "inverted validity always renews");
    }

    #[test]
    fn next_check_delay_bounds() {
        let now = 10_000_000;
        let cert = IssuedCert {
            host: String::new(),
            names: Vec::new(),
            cert_pem: String::new(),
            key_pem: String::new(),
            not_before: now,
            not_after: now + 90 * DAY,
        };
        assert_eq!(next_check_delay(&cert, now, false), RENEW_CHECK_INTERVAL);
        assert_eq!(
            next_check_delay(&cert, now + 45 * DAY - 120, false),
            Duration::from_secs(120)
        );
        assert_eq!(
            next_check_delay(&cert, now + 60 * DAY, false),
            MIN_CHECK_DELAY
        );
        assert_eq!(next_check_delay(&cert, now, true), RETRY_AFTER_FAILURE);
    }

    #[test]
    fn parse_cert_reads_dns_san() {
        let (cert, key) = self_signed("yarmiplay.duckdns.org", (2026, 9, 1), (2026, 11, 30));
        let parsed = parse_cert(&cert, &key, "YarmiPlay.duckdns.org").unwrap();
        assert_eq!(parsed.not_after - parsed.not_before, 90 * DAY);
        assert!(parse_cert(&cert, &key, "other.duckdns.org").is_err());
        assert!(parse_cert(&cert, "nope", "yarmiplay.duckdns.org").is_err());
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let o = opts(dir.path());
        assert!(load_cached(&o).is_none());
        let (cert, key) = self_signed("yarmiplay.duckdns.org", (2026, 1, 1), (2099, 1, 1));
        save_cert(&o, &cert, &key).unwrap();
        assert!(dir
            .path()
            .join("staging")
            .join("yarmiplay.duckdns.org")
            .join(CERT_FILE)
            .is_file());
        assert!(!load_cached(&o).unwrap().needs_renewal(unix_now()));
        let (old_cert, old_key) = self_signed("yarmiplay.duckdns.org", (2020, 1, 1), (2020, 3, 1));
        save_cert(&o, &old_cert, &old_key).unwrap();
        assert!(load_cached(&o).unwrap().needs_renewal(unix_now()));
    }

    async fn fake_duckdns() -> (String, Arc<Mutex<Vec<HashMap<String, String>>>>) {
        let seen: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();
        let log = seen.clone();
        let base = serve(Arc::new(move |req: Request| {
            let ok = req.path == "/update"
                && req.query.get("domains").map(String::as_str) == Some("yarmiplay")
                && req.query.get("token").map(String::as_str) == Some(TEST_TOKEN);
            let body = match (ok, req.query.get("ipv6")) {
                (false, _) => "KO".to_string(),
                (true, Some(v6)) => format!("OK\n203.0.113.10\n{v6}\nUPDATED"),
                (true, None) => "OK\n203.0.113.10\n\nUPDATED".to_string(),
            };
            log.lock().push(req.query);
            (200, body)
        }))
        .await;
        (base, seen)
    }

    #[tokio::test]
    async fn duckdns_api_sets_ip_txt_and_clears() {
        let (api, seen) = fake_duckdns().await;
        let duck = DuckDns::new("yarmiplay.duckdns.org", TEST_TOKEN).unwrap();
        let rec = duckdns_set_ip(&api, &duck, Some("203.0.113.10".parse().unwrap()), None)
            .await
            .unwrap();
        assert_eq!(rec.ip, Some("203.0.113.10".parse().unwrap()));
        assert_eq!(rec.ipv6, None);
        let rec = duckdns_set_ip(&api, &duck, None, Some("2a02:6b8::1".parse().unwrap()))
            .await
            .unwrap();
        assert_eq!(rec.ipv6, Some("2a02:6b8::1".parse().unwrap()));
        duckdns_set_txt(&api, &duck, "abc-DNS01-value")
            .await
            .unwrap();
        duckdns_clear_txt(&api, &duck).await.unwrap();
        let calls = seen.lock().clone();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].get("ip").map(String::as_str), Some("203.0.113.10"));
        assert!(
            !calls[0].contains_key("ipv6"),
            "no IPv6 leaves the AAAA alone"
        );
        assert!(
            !calls[1].contains_key("ip"),
            "None lets DuckDNS auto-detect"
        );
        assert_eq!(
            calls[1].get("ipv6").map(String::as_str),
            Some("2a02:6b8::1")
        );
        assert_eq!(
            calls[2].get("txt").map(String::as_str),
            Some("abc-DNS01-value")
        );
        assert_eq!(calls[3].get("clear").map(String::as_str), Some("true"));
    }

    #[tokio::test]
    async fn duckdns_ko_and_errors_never_leak_token() {
        let (api, _) = fake_duckdns().await;
        let wrong = DuckDns::new("yarmiplay", "wrong-token-value-9999").unwrap();
        let err = duckdns_set_txt(&api, &wrong, "v").await.unwrap_err();
        assert!(
            err.contains("KO") && err.contains("yarmiplay.duckdns.org"),
            "{err}"
        );
        assert!(!err.contains("wrong-token-value-9999"));

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let duck = DuckDns::new("yarmiplay", TEST_TOKEN).unwrap();
        let err = duckdns_set_ip(&dead, &duck, None, None).await.unwrap_err();
        assert!(err.starts_with("DuckDNS request failed"), "{err}");
        assert!(!err.contains(TEST_TOKEN), "token leaked: {err}");
        assert!(!format!("{:?}", opts(Path::new("acme"))).contains(TEST_TOKEN));
    }

    #[test]
    fn two_names_one_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = opts(dir.path());
        assert_eq!(o.names(), vec!["yarmiplay.duckdns.org".to_string()]);
        set_lan(&mut o, Some(DuckDns::new("yarmiplay-lan", TEST_TOKEN).unwrap()));
        assert_eq!(
            o.names(),
            vec![
                "yarmiplay.duckdns.org".to_string(),
                "yarmiplay-lan.duckdns.org".to_string()
            ]
        );
        // Each name's TXT record goes on its own DuckDNS name.
        assert_eq!(
            o.duck_for("YARMIPLAY-LAN.duckdns.org").unwrap().subdomain,
            "yarmiplay-lan"
        );
        assert_eq!(
            o.duck_for("yarmiplay.duckdns.org").unwrap().subdomain,
            "yarmiplay"
        );
        assert!(o.duck_for("other.duckdns.org").is_none());

        // A certificate for both names reads both; one without the LAN name is replaced.
        let params = rcgen::CertificateParams::new(vec![
            "yarmiplay.duckdns.org".into(),
            "yarmiplay-lan.duckdns.org".into(),
        ])
        .unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let both = params.self_signed(&key).unwrap();
        let both = parse_cert(&both.pem(), &key.serialize_pem(), "yarmiplay.duckdns.org").unwrap();
        assert_eq!(
            both.names,
            vec!["yarmiplay.duckdns.org", "yarmiplay-lan.duckdns.org"]
        );
        let now = both.not_before + 60;
        assert!(cached_is_enough(&both, &o.names(), now));
        let (one, one_key) = self_signed("yarmiplay.duckdns.org", (2026, 1, 1), (2099, 1, 1));
        let one = parse_cert(&one, &one_key, "yarmiplay.duckdns.org").unwrap();
        assert!(
            !cached_is_enough(&one, &o.names(), now),
            "the LAN name is missing"
        );
        set_lan(&mut o, None);
        assert!(cached_is_enough(&one, &o.names(), now));
    }

    #[test]
    fn duckdns_domain_normalization() {
        for input in [
            "yarmiplay",
            "YarmiPlay.duckdns.org",
            " yarmiplay.duckdns.org. ",
        ] {
            let d = DuckDns::new(input, " t ").unwrap();
            assert_eq!(d.fqdn(), "yarmiplay.duckdns.org");
            assert_eq!(d.token, "t");
        }
        for bad in ["", ".duckdns.org", "bad name", "-x", "a.b"] {
            assert!(
                DuckDns::new(bad, "t").is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(DuckDns::new("yarmiplay", "  ").is_err());
    }

    #[test]
    fn own_domain_normalization() {
        assert_eq!(normalize_own_domain(" TV.Example.com. ").unwrap(), "tv.example.com");
        assert_eq!(normalize_own_domain("my-home.co.uk").unwrap(), "my-home.co.uk");
        for bad in ["", "example", "bad name.com", "-x.com", "a..com", "1.2.3.4", "http://x.com"] {
            assert!(normalize_own_domain(bad).is_err(), "{bad:?} should be rejected");
        }
        let o = AcmeOptions {
            validation: Validation::Http { domain: "tv.example.com".into() },
            ..opts(Path::new("acme"))
        };
        assert_eq!(o.names(), vec!["tv.example.com".to_string()]);
        assert!(o.duck_for("tv.example.com").is_none());
        assert_eq!(o.cert_dir(), Path::new("acme").join("staging").join("tv.example.com"));
    }

    #[test]
    fn http01_answers_only_known_tokens() {
        let tokens: HashMap<String, String> = [("abc".to_string(), "abc.key".to_string())].into();
        let req = |line: &str| http01_answer(&format!("{line}\r\nHost: x\r\n\r\n"), &tokens);
        assert_eq!(req("GET /.well-known/acme-challenge/abc HTTP/1.1").as_deref(), Some("abc.key"));
        assert_eq!(req("GET /.well-known/acme-challenge/nope HTTP/1.1"), None);
        assert_eq!(req("GET / HTTP/1.1"), None);
        assert_eq!(req("POST /.well-known/acme-challenge/abc HTTP/1.1"), None);
    }

    #[tokio::test]
    async fn http01_server_serves_the_key_authorization() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let server = Http01Server::start(port).await.unwrap();
        server.add("tok".into(), "tok.thumb".into());
        let client = reqwest::Client::new();
        let url = |t: &str| format!("http://127.0.0.1:{port}/.well-known/acme-challenge/{t}");
        let ok = client.get(url("tok")).send().await.unwrap();
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.text().await.unwrap(), "tok.thumb");
        assert_eq!(client.get(url("other")).send().await.unwrap().status(), 404);
        drop(server);
    }

    #[test]
    fn doh_address_parsing() {
        let json = serde_json::json!({
            "Status": 0,
            "Answer": [
                { "name": "tv.example.com.", "type": 5, "data": "home.duckdns.org." },
                { "name": "home.duckdns.org.", "type": 1, "data": "203.0.113.7" }
            ]
        });
        assert_eq!(doh_addresses(&json), vec!["203.0.113.7".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn doh_answer_parsing() {
        let json = serde_json::json!({
            "Status": 0,
            "Answer": [
                { "name": "_acme-challenge.yarmiplay.duckdns.org.", "type": 16, "TTL": 60, "data": "\"abc\" \"def\"" },
                { "name": "x", "type": 5, "data": "cname.example." }
            ]
        });
        assert_eq!(doh_txt_values(&json), vec!["abcdef".to_string()]);
    }

    /// Real staging issuance for yarmiplay.duckdns.org:
    /// `YARMIPLAYSERVERTV_DUCKDNS_TOKEN=... cargo test live_staging_issue -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_staging_issue() {
        let token = std::env::var("YARMIPLAYSERVERTV_DUCKDNS_TOKEN").expect("token env var");
        crate::logs::register_secret(&token);
        let dir = std::env::temp_dir().join("yarmiplayservertv-acme-live");
        let o = AcmeOptions {
            dir: dir.clone(),
            staging: true,
            email: None,
            validation: Validation::DuckDns {
                duck: DuckDns::new("yarmiplay", &token).unwrap(),
                lan: None,
            },
            duckdns_api: None,
            directory: None,
            http01_port: None,
        };
        let ip = duckdns_set_ip(o.duckdns_api(), o.validation.duckdns().unwrap(), None, None)
            .await
            .expect("duckdns ip update");
        eprintln!("duckdns ip {ip:?}");
        let cert = load_or_issue(&o, true).await.expect("staging issuance");
        eprintln!("issued for {} valid until {}", cert.host, cert.not_after);
        assert_eq!(cert.host, "yarmiplay.duckdns.org");
        assert!(crate::tls::server_config(&cert.cert_pem, &cert.key_pem).is_ok());
        assert!(
            crate::jellyfin::netconfig::pem_to_pfx(&cert.cert_pem, &cert.key_pem, "pw").is_ok()
        );
    }
}
