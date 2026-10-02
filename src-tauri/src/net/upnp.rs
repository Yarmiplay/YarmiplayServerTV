//! IGD / UPnP TCP port mapping, ported from GoogleSnakeOnline's `upnp.rs`.
//!
//! Direct SSDP + SOAP: M-SEARCH is sent from every LAN IPv4 adapter, the first
//! gateway exposing WANIPConnection (or WANPPPConnection) wins, and the adapter
//! that heard the reply is the mapping's internal client. Mappings are
//! permanent (lease 0), re-checked every few minutes so they come back after a
//! router reboot, and always deleted on stop or quit.
//!
//! Nothing in here runs unless the user turns UPnP on for a service.

use serde::Serialize;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{info, warn};

/// Fixed IGD mapping description shown in the router UI; also how we recognise our own entries.
pub const MAPPING_DESCRIPTION: &str = "YarmiplayServerTV";

const SSDP_MULTICAST: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const SEARCH_TARGETS: [&str; 2] = [
    "ssdp:all",
    "urn:schemas-upnp-org:device:InternetGatewayDevice:1",
];

/// UPnP error codes (UPnP IGD WANIPConnection spec).
const ERR_ARRAY_INDEX_INVALID: u32 = 713;
const ERR_NO_SUCH_ENTRY: u32 = 714;
const ERR_CONFLICT_IN_MAPPING: u32 = 718;

#[derive(Debug, Error)]
pub enum UpnpError {
    #[error("UPnP SOAP fault {action}: {description} (code {})", code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))]
    Soap {
        action: String,
        code: Option<u32>,
        description: String,
    },
    #[error("{0}")]
    Other(String),
}

impl UpnpError {
    fn code(&self) -> Option<u32> {
        match self {
            UpnpError::Soap { code, .. } => *code,
            UpnpError::Other(_) => None,
        }
    }
}

/// Resolved IGD WAN connection service plus the LAN adapter that reached it.
#[derive(Debug, Clone)]
pub struct Gateway {
    pub location: String,
    pub control_url: String,
    pub service_type: String,
    pub local_ip: Ipv4Addr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingOutcome {
    /// We created (or replaced our own stale) entry; we delete it on stop.
    Added,
    /// Someone (usually the user, on the router page) already forwards this
    /// port to this PC; we leave it alone and never delete it.
    AlreadyForwarded,
}

fn http_client() -> Result<reqwest::Client, String> {
    super::install_crypto_provider();
    crate::net::client_builder()
        .no_proxy()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("UPnP HTTP client: {e}"))
}

// ---------------------------------------------------------------- manager

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MappingStatus {
    pub port: u16,
    pub label: String,
    /// `ok`, `manual` (already forwarded by someone else to this PC) or `error`.
    pub state: &'static str,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpnpStatus {
    pub active: bool,
    pub gateway: Option<String>,
    pub local_ip: Option<String>,
    pub external_ip: Option<String>,
    pub double_nat: bool,
    pub mappings: Vec<MappingStatus>,
    pub error: Option<String>,
}

#[derive(Default)]
struct Inner {
    gateway: Option<Gateway>,
    /// Ports we created and must delete again.
    owned: BTreeMap<u16, String>,
}

/// Keeps the router's port mappings equal to the desired set.
#[derive(Default)]
pub struct UpnpManager {
    inner: tokio::sync::Mutex<Inner>,
    status: parking_lot::RwLock<UpnpStatus>,
}

impl UpnpManager {
    pub fn status(&self) -> UpnpStatus {
        self.status.read().clone()
    }

    pub fn external_ip(&self) -> Option<IpAddr> {
        self.status
            .read()
            .external_ip
            .as_deref()
            .and_then(|s| s.parse().ok())
    }

    /// Make the router's mappings match `desired` (port -> label). With an
    /// empty desired set and nothing owned this does no network I/O at all.
    pub async fn reconcile(&self, desired: BTreeMap<u16, String>) {
        let mut inner = self.inner.lock().await;
        self.apply(&mut inner, &desired, false).await;
    }

    /// Periodic check: rediscover the gateway (it may have rebooted) and re-add missing mappings.
    pub async fn recheck(&self, desired: BTreeMap<u16, String>) {
        let mut inner = self.inner.lock().await;
        if desired.is_empty() && inner.owned.is_empty() {
            return;
        }
        inner.gateway = None;
        self.apply(&mut inner, &desired, true).await;
    }

    /// Delete everything we created (quit / all UPnP switches off).
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        self.apply(&mut inner, &BTreeMap::new(), false).await;
    }

    async fn apply(&self, inner: &mut Inner, desired: &BTreeMap<u16, String>, verify: bool) {
        if desired.is_empty() && inner.owned.is_empty() {
            *self.status.write() = UpnpStatus::default();
            return;
        }
        let client = match http_client() {
            Ok(c) => c,
            Err(e) => return self.fail(desired, e),
        };

        let gateway = match inner.gateway.clone() {
            Some(g) => g,
            None => match discover_gateway_with(&client).await {
                Ok(g) => {
                    inner.gateway = Some(g.clone());
                    g
                }
                Err(e) => return self.fail(desired, e),
            },
        };

        let external_ip = match external_ip_with(&client, &gateway).await {
            Ok(ip) => Some(ip),
            Err(e) => {
                warn!(error = %e, "GetExternalIPAddress failed");
                None
            }
        };
        let double_nat = matches!(external_ip, Some(IpAddr::V4(v4)) if is_private_or_cgnat(v4));
        if double_nat {
            warn!(?external_ip, "router WAN address is private/CGNAT (double NAT); internet peers may not reach these forwards");
        }

        let stale: Vec<u16> = inner
            .owned
            .keys()
            .filter(|p| !desired.contains_key(p))
            .copied()
            .collect();
        for port in stale {
            match delete_mapping_with(&client, &gateway, port).await {
                Ok(()) => info!(port, "UPnP mapping removed"),
                Err(e) => warn!(port, error = %e, "UPnP mapping removal failed"),
            }
            inner.owned.remove(&port);
        }

        let mut mappings = Vec::new();
        for (&port, label) in desired {
            let already = inner.owned.contains_key(&port);
            if already && !verify {
                mappings.push(MappingStatus {
                    port,
                    label: label.clone(),
                    state: "ok",
                    error: None,
                });
                continue;
            }
            if already && verify {
                if let Ok(Some(entry)) = get_mapping_with(&client, &gateway, port).await {
                    let ours = entry.get("NewInternalClient").map(String::as_str)
                        == Some(&gateway.local_ip.to_string());
                    if ours {
                        mappings.push(MappingStatus {
                            port,
                            label: label.clone(),
                            state: "ok",
                            error: None,
                        });
                        continue;
                    }
                }
                info!(port, "UPnP mapping missing after re-check, adding it again");
            }
            match ensure_mapping_with(&client, &gateway, port, port).await {
                Ok(MappingOutcome::Added) => {
                    info!(port, %label, local_ip = %gateway.local_ip, "UPnP mapping added");
                    inner.owned.insert(port, label.clone());
                    mappings.push(MappingStatus {
                        port,
                        label: label.clone(),
                        state: "ok",
                        error: None,
                    });
                }
                Ok(MappingOutcome::AlreadyForwarded) => {
                    info!(port, "port already forwarded to this PC by another router entry; leaving it as is");
                    mappings.push(MappingStatus {
                        port,
                        label: label.clone(),
                        state: "manual",
                        error: None,
                    });
                }
                Err(e) => {
                    warn!(port, error = %e, "UPnP mapping failed");
                    mappings.push(MappingStatus {
                        port,
                        label: label.clone(),
                        state: "error",
                        error: Some(e.to_string()),
                    });
                }
            }
        }

        let active = !desired.is_empty();
        *self.status.write() = UpnpStatus {
            active,
            gateway: Some(gateway.location.clone()),
            local_ip: Some(gateway.local_ip.to_string()),
            external_ip: external_ip.map(|ip| ip.to_string()),
            double_nat,
            mappings,
            error: None,
        };
        if !active {
            inner.gateway = None;
        }
    }

    fn fail(&self, desired: &BTreeMap<u16, String>, error: String) {
        warn!(%error, "UPnP unavailable");
        *self.status.write() = UpnpStatus {
            active: !desired.is_empty(),
            mappings: desired
                .iter()
                .map(|(&port, label)| MappingStatus {
                    port,
                    label: label.clone(),
                    state: "error",
                    error: Some(error.clone()),
                })
                .collect(),
            error: Some(error),
            ..Default::default()
        };
    }
}

// ---------------------------------------------------------------- discovery

/// Non-loopback, non-link-local IPv4 adapters.
pub fn lan_ipv4_adapters() -> Vec<(String, Ipv4Addr)> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out: Vec<(String, Ipv4Addr)> = Vec::new();
    for iface in ifaces {
        if iface.is_loopback() {
            continue;
        }
        if let IpAddr::V4(ip) = iface.ip() {
            if ip.is_link_local() || ip.is_unspecified() || out.iter().any(|(_, a)| *a == ip) {
                continue;
            }
            out.push((iface.name.clone(), ip));
        }
    }
    out
}

fn search_message(st: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_MULTICAST}:{SSDP_PORT}\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
    )
}

fn ssdp_socket(ip: Ipv4Addr) -> std::io::Result<tokio::net::UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.bind(&SocketAddr::V4(SocketAddrV4::new(ip, 0)).into())?;
    socket.set_multicast_if_v4(&ip)?;
    socket.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(socket.into())
}

struct SsdpHit {
    location: String,
    adapter: String,
    local_ip: Ipv4Addr,
}

async fn discover_gateway_with(client: &reqwest::Client) -> Result<Gateway, String> {
    let adapters = lan_ipv4_adapters();
    if adapters.is_empty() {
        return Err("no LAN IPv4 adapter found for UPnP discovery".into());
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<SsdpHit>();
    let mut listeners = JoinSet::new();
    for (name, ip) in &adapters {
        let socket = match ssdp_socket(*ip) {
            Ok(s) => s,
            Err(e) => {
                warn!(adapter = %name, %ip, error = %e, "SSDP socket failed on adapter");
                continue;
            }
        };
        let target = SocketAddr::V4(SocketAddrV4::new(SSDP_MULTICAST, SSDP_PORT));
        for st in SEARCH_TARGETS {
            if let Err(e) = socket.send_to(search_message(st).as_bytes(), target).await {
                warn!(adapter = %name, %ip, error = %e, "SSDP send failed");
            }
        }
        let tx = tx.clone();
        let adapter = name.clone();
        let local_ip = *ip;
        listeners.spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, _from)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let text = String::from_utf8_lossy(&buf[..n]);
                let headers = parse_ssdp_headers(&text);
                if !is_gateway_response(&headers) {
                    continue;
                }
                if let Some(location) = headers.get("location") {
                    let _ = tx.send(SsdpHit {
                        location: location.clone(),
                        adapter: adapter.clone(),
                        local_ip,
                    });
                }
            }
        });
    }
    drop(tx);

    let deadline = tokio::time::Instant::now() + DISCOVERY_TIMEOUT;
    let mut tried = HashSet::new();
    let mut last_err: Option<String> = None;
    while let Ok(Some(hit)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        if !tried.insert(hit.location.clone()) {
            continue;
        }
        match resolve_gateway(client, &hit.location, hit.local_ip).await {
            Ok(gateway) => {
                info!(adapter = %hit.adapter, local_ip = %gateway.local_ip, location = %gateway.location, "UPnP gateway found");
                listeners.abort_all();
                return Ok(gateway);
            }
            Err(e) => {
                warn!(location = %hit.location, error = %e, "UPnP gateway rejected");
                last_err = Some(format!("{}: {e}", hit.location));
            }
        }
    }
    listeners.abort_all();

    let tried_adapters = adapters
        .iter()
        .map(|(n, ip)| format!("{n} ({ip})"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(match last_err {
        Some(e) => format!("no usable UPnP gateway (last: {e}); adapters tried: {tried_adapters}"),
        None => format!(
            "no UPnP router answered within {}s (is UPnP enabled on the router?); adapters tried: {tried_adapters}",
            DISCOVERY_TIMEOUT.as_secs()
        ),
    })
}

fn parse_ssdp_headers(message: &str) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for line in message.lines() {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    headers
}

fn is_gateway_response(headers: &HashMap<String, String>) -> bool {
    const MARKERS: [&str; 3] = [
        "InternetGatewayDevice",
        "WANIPConnection",
        "WANPPPConnection",
    ];
    ["st", "usn"].iter().any(|key| {
        headers
            .get(*key)
            .is_some_and(|v| MARKERS.iter().any(|m| v.contains(m)))
    })
}

async fn resolve_gateway(
    client: &reqwest::Client,
    location: &str,
    local_ip: Ipv4Addr,
) -> Result<Gateway, String> {
    let response = client
        .get(location)
        .send()
        .await
        .map_err(|e| format!("description fetch failed: {e}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("description read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("description HTTP {status}"));
    }
    let (service_type, control_url) = find_wan_service(&body, location)?;
    Ok(Gateway {
        location: location.to_string(),
        control_url,
        service_type,
        local_ip,
    })
}

// ---------------------------------------------------------------- XML / SOAP

fn parse_xml(xml: &str) -> Result<roxmltree::Document<'_>, String> {
    let trimmed = xml.trim_start_matches('\u{feff}').trim_start();
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    roxmltree::Document::parse_with_options(trimmed, options).map_err(|e| format!("bad XML: {e}"))
}

fn child_text<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .map(str::trim)
}

/// Find the WAN connection service in an IGD description.
/// Returns `(serviceType, absolute controlURL)`; WANIPConnection preferred.
fn find_wan_service(xml: &str, location: &str) -> Result<(String, String), String> {
    let doc = parse_xml(xml)?;
    let root = doc.root_element();
    let base = child_text(root, "URLBase")
        .filter(|s| !s.is_empty())
        .unwrap_or(location);

    let mut ppp: Option<(String, String)> = None;
    for node in root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "service")
    {
        let (Some(st), Some(ctrl)) = (
            child_text(node, "serviceType"),
            child_text(node, "controlURL"),
        ) else {
            continue;
        };
        let entry = (st.to_string(), ctrl.to_string());
        if st.contains(":service:WANIPConnection:") {
            return absolutize(base, entry);
        }
        if ppp.is_none() && st.contains(":service:WANPPPConnection:") {
            ppp = Some(entry);
        }
    }
    match ppp {
        Some(entry) => absolutize(base, entry),
        None => Err("no WANIPConnection/WANPPPConnection service in description".into()),
    }
}

fn absolutize(base: &str, (st, ctrl): (String, String)) -> Result<(String, String), String> {
    let url = reqwest::Url::parse(base)
        .and_then(|b| b.join(&ctrl))
        .map_err(|e| format!("bad controlURL {ctrl:?} (base {base:?}): {e}"))?;
    Ok((st, url.to_string()))
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn soap_envelope(service_type: &str, action: &str, args: &[(&str, String)]) -> String {
    let body: String = args
        .iter()
        .map(|(name, value)| format!("<{name}>{}</{name}>", escape_xml(value)))
        .collect();
    format!(
        "<?xml version=\"1.0\"?>\
<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
<s:Body><u:{action} xmlns:u=\"{}\">{body}</u:{action}></s:Body></s:Envelope>",
        escape_xml(service_type)
    )
}

fn parse_soap_response(xml: &str, action: &str) -> Result<HashMap<String, String>, UpnpError> {
    let doc = parse_xml(xml).map_err(UpnpError::Other)?;
    let body = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "Body")
        .ok_or_else(|| UpnpError::Other(format!("{action}: SOAP Body not found")))?;

    if let Some(fault) = body
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "Fault")
    {
        let find = |name: &str| {
            fault
                .descendants()
                .find(|n| n.is_element() && n.tag_name().name() == name)
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
        };
        let code = find("errorCode").and_then(|c| c.parse::<u32>().ok());
        let description = find("errorDescription")
            .or_else(|| find("faultstring"))
            .unwrap_or_else(|| "unknown SOAP fault".into());
        return Err(UpnpError::Soap {
            action: action.to_string(),
            code,
            description,
        });
    }

    let response_name = format!("{action}Response");
    let response = body
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == response_name)
        .ok_or_else(|| UpnpError::Other(format!("{response_name} not found in SOAP body")))?;
    Ok(response
        .children()
        .filter(|n| n.is_element())
        .map(|n| {
            (
                n.tag_name().name().to_string(),
                n.text().unwrap_or("").trim().to_string(),
            )
        })
        .collect())
}

async fn soap_call(
    client: &reqwest::Client,
    gateway: &Gateway,
    action: &str,
    args: &[(&str, String)],
) -> Result<HashMap<String, String>, UpnpError> {
    let response = client
        .post(&gateway.control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header(
            "SOAPAction",
            format!("\"{}#{action}\"", gateway.service_type),
        )
        .body(soap_envelope(&gateway.service_type, action, args))
        .send()
        .await
        .map_err(|e| UpnpError::Other(format!("{action} request failed: {e}")))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| UpnpError::Other(format!("{action} read failed: {e}")))?;
    if !status.is_success() && status.as_u16() != 500 {
        return Err(UpnpError::Other(format!("{action}: HTTP {status}")));
    }
    parse_soap_response(&text, action)
}

// ---------------------------------------------------------------- actions

async fn external_ip_with(
    client: &reqwest::Client,
    gateway: &Gateway,
) -> Result<IpAddr, UpnpError> {
    let fields = soap_call(client, gateway, "GetExternalIPAddress", &[]).await?;
    let raw = fields
        .get("NewExternalIPAddress")
        .map(String::as_str)
        .unwrap_or("");
    match raw.parse::<IpAddr>() {
        Ok(ip) if !ip.is_unspecified() => Ok(ip),
        _ => Err(UpnpError::Other(format!(
            "router reported no usable external IP ({raw:?}); WAN may be down"
        ))),
    }
}

async fn get_mapping_with(
    client: &reqwest::Client,
    gateway: &Gateway,
    external_port: u16,
) -> Result<Option<HashMap<String, String>>, UpnpError> {
    let args = [
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", external_port.to_string()),
        ("NewProtocol", "TCP".to_string()),
    ];
    match soap_call(client, gateway, "GetSpecificPortMappingEntry", &args).await {
        Ok(fields) => Ok(Some(fields)),
        Err(e) if matches!(e.code(), Some(ERR_NO_SUCH_ENTRY | ERR_ARRAY_INDEX_INVALID)) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn delete_mapping_with(
    client: &reqwest::Client,
    gateway: &Gateway,
    external_port: u16,
) -> Result<(), UpnpError> {
    let args = [
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", external_port.to_string()),
        ("NewProtocol", "TCP".to_string()),
    ];
    match soap_call(client, gateway, "DeletePortMapping", &args).await {
        Ok(_) => Ok(()),
        Err(e) if e.code() == Some(ERR_NO_SUCH_ENTRY) => Ok(()),
        Err(e) => Err(e),
    }
}

async fn add_mapping_with(
    client: &reqwest::Client,
    gateway: &Gateway,
    external_port: u16,
    internal_port: u16,
) -> Result<(), UpnpError> {
    let args = [
        ("NewRemoteHost", String::new()),
        ("NewExternalPort", external_port.to_string()),
        ("NewProtocol", "TCP".to_string()),
        ("NewInternalPort", internal_port.to_string()),
        ("NewInternalClient", gateway.local_ip.to_string()),
        ("NewEnabled", "1".to_string()),
        ("NewPortMappingDescription", MAPPING_DESCRIPTION.to_string()),
        ("NewLeaseDuration", "0".to_string()),
    ];
    soap_call(client, gateway, "AddPortMapping", &args)
        .await
        .map(|_| ())
}

/// Look up the existing entry first. Our own stale entries are replaced; an
/// entry from another app or the router page is left alone: fine if it already
/// points at this PC and port, otherwise an error explaining the conflict.
async fn ensure_mapping_with(
    client: &reqwest::Client,
    gateway: &Gateway,
    external_port: u16,
    internal_port: u16,
) -> Result<MappingOutcome, UpnpError> {
    match get_mapping_with(client, gateway, external_port).await {
        Ok(Some(existing)) => {
            let internal_client = existing
                .get("NewInternalClient")
                .map(String::as_str)
                .unwrap_or("?");
            let existing_port = existing
                .get("NewInternalPort")
                .map(String::as_str)
                .unwrap_or("?");
            let description = existing
                .get("NewPortMappingDescription")
                .map(String::as_str)
                .unwrap_or("?");
            if description != MAPPING_DESCRIPTION {
                if internal_client == gateway.local_ip.to_string()
                    && existing_port == internal_port.to_string()
                {
                    return Ok(MappingOutcome::AlreadyForwarded);
                }
                return Err(UpnpError::Other(format!(
                    "external TCP {external_port} is already forwarded to {internal_client}:{existing_port} ({description:?}); remove that router mapping or pick another port"
                )));
            }
            delete_mapping_with(client, gateway, external_port).await?;
        }
        Ok(None) => {}
        Err(e) => warn!(external_port, error = %e, "GetSpecificPortMappingEntry failed"),
    }

    match add_mapping_with(client, gateway, external_port, internal_port).await {
        Err(e) if e.code() == Some(ERR_CONFLICT_IN_MAPPING) => {
            delete_mapping_with(client, gateway, external_port).await?;
            add_mapping_with(client, gateway, external_port, internal_port).await
        }
        other => other,
    }
    .map(|()| MappingOutcome::Added)
}

/// RFC 1918 private or RFC 6598 carrier-grade NAT (100.64.0.0/10).
pub fn is_private_or_cgnat(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || (a == 100 && (64..128).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use parking_lot::Mutex;
    use std::sync::Arc;

    const IGD_DESC: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <device>
    <deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
    <deviceList><device>
      <deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
      <deviceList><device>
        <deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
        <serviceList>
          <service>
            <serviceType>urn:schemas-upnp-org:service:WANPPPConnection:1</serviceType>
            <controlURL>/ctl/PPP</controlURL>
          </service>
          <service>
            <serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
            <controlURL>/ctl/IPConn</controlURL>
          </service>
        </serviceList>
      </device></deviceList>
    </device></deviceList>
  </device>
</root>"#;

    #[test]
    fn ssdp_headers_parse_and_gateway_match() {
        let reply = "HTTP/1.1 200 OK\r\nLocation: http://192.168.1.1:5000/rootDesc.xml\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\nUSN: uuid:abc::urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
        let h = parse_ssdp_headers(reply);
        assert_eq!(
            h.get("location").unwrap(),
            "http://192.168.1.1:5000/rootDesc.xml"
        );
        assert!(is_gateway_response(&h));
        let chromecast = parse_ssdp_headers(
            "HTTP/1.1 200 OK\r\nST: urn:dial-multiscreen-org:service:dial:1\r\nUSN: uuid:x::urn:dial-multiscreen-org:service:dial:1\r\nLOCATION: http://192.168.1.9:8008/ssdp/device-desc.xml\r\n",
        );
        assert!(!is_gateway_response(&chromecast));
    }

    #[test]
    fn wan_service_prefers_ip_connection_and_resolves_relative() {
        let (st, ctrl) =
            find_wan_service(IGD_DESC, "http://192.168.1.1:5000/rootDesc.xml").unwrap();
        assert_eq!(st, "urn:schemas-upnp-org:service:WANIPConnection:1");
        assert_eq!(ctrl, "http://192.168.1.1:5000/ctl/IPConn");
    }

    #[test]
    fn soap_envelope_escapes_values() {
        let env = soap_envelope(
            "urn:schemas-upnp-org:service:WANIPConnection:1",
            "AddPortMapping",
            &[("NewPortMappingDescription", "a<b>&\"c'".into())],
        );
        assert!(env.contains(
            "<NewPortMappingDescription>a&lt;b&gt;&amp;&quot;c&apos;</NewPortMappingDescription>"
        ));
        assert!(parse_xml(&env).is_ok());
    }

    #[test]
    fn soap_fault_714_parses_code() {
        let fault = r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring>
<detail><UPnPError xmlns="urn:schemas-upnp-org:control-1-0"><errorCode>714</errorCode><errorDescription>NoSuchEntryInArray</errorDescription></UPnPError></detail>
</s:Fault></s:Body></s:Envelope>"#;
        let err = parse_soap_response(fault, "GetSpecificPortMappingEntry").unwrap_err();
        assert_eq!(err.code(), Some(ERR_NO_SUCH_ENTRY));
    }

    #[test]
    fn private_and_cgnat_detection() {
        assert!(is_private_or_cgnat("192.168.1.5".parse().unwrap()));
        assert!(is_private_or_cgnat("100.64.0.1".parse().unwrap()));
        assert!(!is_private_or_cgnat("100.128.0.1".parse().unwrap()));
        assert!(!is_private_or_cgnat("203.0.113.7".parse().unwrap()));
    }

    #[derive(Default)]
    struct FakeRouter {
        actions: Vec<String>,
        /// (internal client, description) of the current entry.
        entry: Option<(String, String)>,
        last_add_body: String,
    }

    async fn fake_router(router: Arc<Mutex<FakeRouter>>) -> String {
        serve(Arc::new(move |req: Request| {
            if req.method == "GET" {
                return (200, IGD_DESC.to_string());
            }
            let action = req
                .headers
                .get("soapaction")
                .and_then(|v| v.trim_matches('"').split('#').nth(1))
                .unwrap_or("")
                .to_string();
            let mut r = router.lock();
            r.actions.push(action.clone());
            let envelope = |inner: String| {
                format!("<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\"><s:Body>{inner}</s:Body></s:Envelope>")
            };
            let fault = |code: u32| {
                envelope(format!("<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>err</errorDescription></UPnPError></detail></s:Fault>"))
            };
            let ok = |inner: &str| {
                envelope(format!("<u:{action}Response xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\">{inner}</u:{action}Response>"))
            };
            match action.as_str() {
                "GetSpecificPortMappingEntry" => match r.entry.clone() {
                    Some((client, desc)) => (200, ok(&format!("<NewInternalClient>{client}</NewInternalClient><NewInternalPort>8999</NewInternalPort><NewPortMappingDescription>{desc}</NewPortMappingDescription>"))),
                    None => (500, fault(714)),
                },
                "DeletePortMapping" => {
                    if r.entry.take().is_some() { (200, ok("")) } else { (500, fault(714)) }
                }
                "AddPortMapping" => {
                    if r.entry.is_some() {
                        return (500, fault(718));
                    }
                    r.entry = Some(("192.168.1.77".into(), MAPPING_DESCRIPTION.into()));
                    r.last_add_body = req.body.clone();
                    (200, ok(""))
                }
                "GetExternalIPAddress" => (200, ok("<NewExternalIPAddress>203.0.113.7</NewExternalIPAddress>")),
                _ => (400, String::new()),
            }
        }))
        .await
    }

    #[tokio::test]
    async fn ensure_mapping_handles_own_foreign_and_manual_entries() {
        let router = Arc::new(Mutex::new(FakeRouter::default()));
        let base = fake_router(router.clone()).await;
        let client = http_client().unwrap();
        let gw = resolve_gateway(
            &client,
            &format!("{base}/rootDesc.xml"),
            Ipv4Addr::new(192, 168, 1, 77),
        )
        .await
        .unwrap();
        assert_eq!(
            external_ip_with(&client, &gw).await.unwrap(),
            "203.0.113.7".parse::<IpAddr>().unwrap()
        );
        router.lock().actions.clear();

        // Absent: plain add with our description and lease 0.
        assert_eq!(
            ensure_mapping_with(&client, &gw, 8999, 8999).await.unwrap(),
            MappingOutcome::Added
        );
        {
            let r = router.lock();
            assert_eq!(r.actions, ["GetSpecificPortMappingEntry", "AddPortMapping"]);
            assert!(r.last_add_body.contains(
                "<NewPortMappingDescription>YarmiplayServerTV</NewPortMappingDescription>"
            ));
            assert!(r
                .last_add_body
                .contains("<NewLeaseDuration>0</NewLeaseDuration>"));
        }

        // Our own stale entry is replaced.
        router.lock().actions.clear();
        assert_eq!(
            ensure_mapping_with(&client, &gw, 8999, 8999).await.unwrap(),
            MappingOutcome::Added
        );
        assert_eq!(
            router.lock().actions,
            [
                "GetSpecificPortMappingEntry",
                "DeletePortMapping",
                "AddPortMapping"
            ]
        );

        // A manual router entry to this PC is accepted and left alone.
        router.lock().entry = Some(("192.168.1.77".into(), "my forward".into()));
        router.lock().actions.clear();
        assert_eq!(
            ensure_mapping_with(&client, &gw, 8999, 8999).await.unwrap(),
            MappingOutcome::AlreadyForwarded
        );
        assert_eq!(router.lock().actions, ["GetSpecificPortMappingEntry"]);

        // A foreign entry to another PC is an error and is not touched.
        router.lock().entry = Some(("192.168.1.50".into(), "nas".into()));
        let err = ensure_mapping_with(&client, &gw, 8999, 8999)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("192.168.1.50"));
        assert!(router.lock().entry.is_some());
    }

    #[tokio::test]
    async fn manager_does_nothing_when_upnp_is_off() {
        let m = UpnpManager::default();
        m.reconcile(BTreeMap::new()).await;
        assert_eq!(m.status(), UpnpStatus::default());
        m.recheck(BTreeMap::new()).await;
        m.shutdown().await;
        assert!(!m.status().active);
    }
}
