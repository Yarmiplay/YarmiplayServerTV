use std::net::{IpAddr, Ipv4Addr, UdpSocket};

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
