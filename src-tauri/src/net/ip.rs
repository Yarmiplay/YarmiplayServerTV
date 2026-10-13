use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket};

/// The LAN address other devices use to reach this PC: the source address the
/// OS would pick for internet traffic (no packet is sent), falling back to the
/// first private adapter address.
pub fn primary_lan_ipv4() -> Option<Ipv4Addr> {
    let routed = UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .ok()
        .and_then(|a| match a.ip() {
            IpAddr::V4(v4) if !v4.is_unspecified() && !v4.is_loopback() => Some(v4),
            _ => None,
        });
    routed.or_else(|| {
        super::upnp::lan_ipv4_adapters()
            .into_iter()
            .map(|(_, ip)| ip)
            .find(|ip| ip.is_private())
    })
}

/// This PC's public IPv6 address: the source address the OS would pick for
/// internet traffic (no packet is sent), if it is global unicast.
/// Look up a LAN DuckDNS name the way a browser at home would (through
/// this PC's DNS, usually the router) and check it points at this PC.
pub async fn test_lan_name(name: &str) -> Result<String, String> {
    let sub = crate::net::acme::normalize_domain(name)?;
    let fqdn = format!("{sub}.duckdns.org");
    let lan = primary_lan_ipv4().ok_or("This PC has no LAN address right now")?;
    let found: Vec<std::net::IpAddr> = match tokio::net::lookup_host((fqdn.as_str(), 443)).await {
        Ok(addrs) => addrs.map(|a| a.ip()).collect(),
        Err(_) => Vec::new(),
    };
    if found.contains(&std::net::IpAddr::V4(lan)) {
        return Ok(format!("{fqdn} points at this PC ({lan})"));
    }
    if found.is_empty() {
        return Err(format!(
            "{fqdn} doesn't resolve here. If DuckDNS already has {lan} for it, your router is probably blocking              private-address answers (\"DNS rebinding protection\"): add an exception for {fqdn} there."
        ));
    }
    let list: Vec<String> = found.iter().map(|ip| ip.to_string()).collect();
    Err(format!(
        "{fqdn} points at {} instead of this PC ({lan}). Save the LAN name and wait a minute for DuckDNS, then test again.",
        list.join(", ")
    ))
}

pub fn global_ipv6() -> Option<Ipv6Addr> {
    UdpSocket::bind("[::]:0")
        .and_then(|s| {
            s.connect("[2001:4860:4860::8888]:53")?;
            s.local_addr()
        })
        .ok()
        .and_then(|a| match a.ip() {
            IpAddr::V6(v6) if is_global_ipv6(&v6) => Some(v6),
            _ => None,
        })
}

/// Global unicast (`2000::/3`), excluding the documentation range.
pub fn is_global_ipv6(ip: &Ipv6Addr) -> bool {
    let s = ip.segments();
    (s[0] & 0xe000) == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
}

/// `ip:port` for every address devices at home can reach this PC on: the
/// primary LAN address first, the other private adapters, then the public IPv6.
pub fn lan_addresses(port: u16) -> Vec<String> {
    let mut ips: Vec<Ipv4Addr> = primary_lan_ipv4().into_iter().collect();
    for (_, ip) in super::upnp::lan_ipv4_adapters() {
        if ip.is_private() && !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    let mut out: Vec<String> = ips.iter().map(|ip| format!("{ip}:{port}")).collect();
    if let Some(v6) = global_ipv6() {
        out.push(format!("[{v6}]:{port}"));
    }
    out
}

/// Whether a request may come from devices at home (private, link-local,
/// unique-local or loopback).
pub fn is_local_source(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_ipv6_filter() {
        let g = |s: &str| is_global_ipv6(&s.parse().unwrap());
        assert!(g("2a02:6b8::1"));
        assert!(g("2600:1700:abcd::12"));
        assert!(!g("fe80::1"));
        assert!(!g("fd00::1"));
        assert!(!g("::1"));
        assert!(!g("::ffff:192.168.1.2"));
        assert!(!g("2001:db8::1"));
    }

    #[test]
    fn local_sources() {
        let l = |s: &str| is_local_source(s.parse().unwrap());
        assert!(l("192.168.1.5"));
        assert!(l("10.0.0.2"));
        assert!(l("127.0.0.1"));
        assert!(l("::ffff:192.168.1.5"));
        assert!(l("fe80::1"));
        assert!(l("fd12:3456::1"));
        assert!(l("::1"));
        assert!(!l("8.8.8.8"));
        assert!(!l("2a02:6b8::1"));
        assert!(!l("::ffff:8.8.8.8"));
    }
}
