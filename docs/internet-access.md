# YarmiplayServerTV Internet Access Guide

This guide explains how to make your **Syncplay server** and **Jellyfin media server** securely accessible to friends and family outside your home network.

> **Note:** If you and your friends are connected to the same home Wi-Fi or wired local network, you do not need port forwarding or a domain. You can connect directly using your computer's local IP address (e.g. `192.168.1.150:8999`).

---

## Table of contents

- [Overview: LAN vs. internet](#overview-lan-vs-internet)
- [Port forwarding](#port-forwarding)
  - [Default ports](#default-ports)
  - [Automatic forwarding with UPnP](#automatic-forwarding-with-upnp)
  - [Manual port forwarding in your router](#manual-port-forwarding-in-your-router)
- [Double NAT & CGNAT troubleshooting](#double-nat--cgnat-troubleshooting)
- [DuckDNS & free automatic HTTPS](#duckdns--free-automatic-https)
  - [Why HTTPS is important](#why-https-is-important)
  - [Setting up DuckDNS](#setting-up-duckdns)
  - [Automatic certificate renewals](#automatic-certificate-renewals)
- [Using your own custom domain](#using-your-own-custom-domain)
  - [DNS configuration](#dns-configuration)
  - [HTTP-01 validation on port 80](#http-01-validation-on-port-80)
  - [Linux port 80 permissions](#linux-port-80-permissions)
- [Connecting from home (NAT loopback)](#connecting-from-home-nat-loopback)
  - [Automatic local discovery](#automatic-local-discovery)
  - [Direct IPv6 routing](#direct-ipv6-routing)
  - [LAN name for browsers](#lan-name-for-browsers)
- [Watching in a web browser](#watching-in-a-web-browser)

---

## Overview: LAN vs. internet

Your home router creates a private Local Area Network (LAN). Devices inside your home can talk to each other directly using private IP addresses (typically starting with `192.168.x.x` or `10.0.x.x`).

However, people outside your home network connect over the public internet to your router's external IP address. Because routers protect private devices with a Network Address Translation (NAT) firewall, incoming connection requests from friends will be dropped unless your router knows where to forward them.

YarmiplayServerTV gives you two straightforward ways to open these connections:
1. **Automatic:** UPnP (Universal Plug and Play) port forwarding handled entirely by the app.
2. **Manual:** Adding port forwarding rules in your router's administration web panel.

---

## Port forwarding

### Default ports

YarmiplayServerTV utilizes the following TCP ports:

| Service | Port | Protocol | Purpose |
| :--- | :--- | :--- | :--- |
| **Syncplay** | `8999` | TCP | Syncplay rooms, chat, file relay, and Jellyfin proxy |
| **Jellyfin HTTP** | `8096` | TCP | Jellyfin local web interface and media streaming |
| **Jellyfin HTTPS** | `8920` | TCP | Secure Jellyfin streaming with full certificate chain |
| **Plex** | `32400` | TCP | Plex Media Server remote access |
| **ACME HTTP-01** | `80` | TCP | Let's Encrypt validation (custom domains only) |

> **Tip:** You only need to forward ports for the servers you actively use. Official Syncplay clients, YarmiplayTV, and the file relay all operate through the single Syncplay port (`8999`).

### Automatic forwarding with UPnP

Universal Plug and Play (UPnP) allows YarmiplayServerTV to request port forwarding rules directly from your router without manual configuration:

1. Open the YarmiplayServerTV control panel and navigate to the **Network** page.
2. Look for the **UPnP** toggles under each server (Syncplay, Jellyfin, Plex).
3. Switch **UPnP** on. The app will communicate with your router and display a confirmation with your public IP address.

YarmiplayServerTV monitors active mappings every 10 minutes to verify they remain open. When you close the app or switch UPnP off, the app automatically tells your router to remove the forwarded rules.

### Manual port forwarding in your router

If your router has UPnP disabled for security, or if you prefer permanent rules:

1. Look up your computer's local IP address on the **Network** page (e.g. `192.168.1.45`).
2. Open your web browser and navigate to your router's admin gateway (usually `192.168.1.1` or `192.168.0.1`).
3. Find the **Port Forwarding** (or **Virtual Server**) section in your router settings.
4. Add a rule:
   - **Service Name:** `Yarmiplay Syncplay`
   - **Internal IP:** Your computer's local IP address
   - **Protocol:** `TCP`
   - **External Port:** `8999`
   - **Internal Port:** `8999`
5. If running Jellyfin independently, add a second rule for TCP `8096` (and `8920` for HTTPS).
6. Save and apply the router settings.

---

## Double NAT & CGNAT troubleshooting

If you turn on UPnP and the Network page reports **Double NAT**, your incoming internet traffic is passing through multiple layers of network translation before reaching your computer.

Common causes:
- **Two routers in series:** Your computer is connected to a personal Wi-Fi router (e.g. ASUS, Netgear, or Google Nest) which is plugged into your internet provider's modem/router combo unit.
  - *Fix:* Set your provider modem into **Bridge Mode** (IP Passthrough), or forward the ports on the primary modem to your secondary router's WAN IP.
- **Carrier-Grade NAT (CGNAT):** Many cellular home internet providers (5G/LTE), satellite services (Starlink), and fiber providers share a single public IP among hundreds of homes.
  - *Fix:* Contact your ISP and request a **public dynamic IPv4 address** or a **static IP** (often called "gamer IP" or "public IP add-on"). Alternatively, enable **IPv6**, which bypasses NAT entirely.

---

## DuckDNS & free automatic HTTPS

### Why HTTPS is important

By default, network traffic over the internet is unencrypted. When you enable HTTPS / TLS:
- All Syncplay room chat, playback sync, and credentials are encrypted.
- Friends can join directly from modern web browsers at [tv.yarmiplay.com/watch](https://tv.yarmiplay.com/watch/), which requires secure WebSocket (`wss://`) connections.
- Your server receives a trusted SSL/TLS certificate issued by Let's Encrypt that devices automatically trust.

### Setting up DuckDNS

DuckDNS is a free dynamic DNS service that gives you a fixed web address (such as `myroom.duckdns.org`) that automatically tracks your home router's public IP address even when your ISP changes it:

1. Visit [duckdns.org](https://www.duckdns.org) and log in with your preferred account.
2. Under **Domains**, pick a subdomain name (for example, `myfamilytv`) and click **Add domain**.
3. Copy your unique **DuckDNS Token** displayed at the top of the DuckDNS webpage.
4. In YarmiplayServerTV, go to the **Network** page and switch on **Use a domain and HTTPS**.
5. Select **DuckDNS** as the domain type, enter your domain name (`myfamilytv.duckdns.org`), and click **Save**.
6. Paste your DuckDNS Token into the token box and click **Save token**.

> **Security Note:** Your DuckDNS token is encrypted and stored in your operating system's native credential store (Windows Credential Manager, macOS Keychain, or Linux Secret Service). It is never logged or exposed.

The app immediately performs a Let's Encrypt DNS challenge:
- It asks DuckDNS to create a temporary TXT record to prove domain ownership.
- **No open router ports are required** for this validation.
- Within 60 seconds, your Let's Encrypt TLS certificate is downloaded and activated for both Syncplay and Jellyfin!

### Automatic certificate renewals

Let's Encrypt certificates are valid for 90 days. YarmiplayServerTV automatically monitors certificate expiration and renews your certificate halfway through its lifetime (at roughly 45 days). Both Syncplay and Jellyfin automatically reload the new certificate without requiring a server restart.

---

## Using your own custom domain

If you own a domain name (such as `stream.example.com`), you can use it instead of DuckDNS.

### DNS configuration

1. At your domain registrar or DNS host (e.g. Cloudflare, Namecheap, Google Domains), create a DNS **A record**:
   - **Name / Host:** Subdomain (e.g. `tv` or `stream`)
   - **Value:** Your public IPv4 address (shown on the **Network** page)
   - **TTL:** 5 minutes (or Auto)
2. If your home IP address changes frequently, you can point a **CNAME record** to a free DuckDNS subdomain that tracks your IP address.

### HTTP-01 validation on port 80

Because Let's Encrypt needs to verify that you control your custom domain, it performs an HTTP-01 challenge over standard **port 80**:
1. When you click **Save**, YarmiplayServerTV briefly opens an internal ACME listener on port `80`.
2. Port 80 must be forwarded to this computer (if UPnP is enabled, the app forwards port 80 automatically during issuance and renewal).
3. Once the certificate is validated and issued, the listener on port 80 shuts down immediately.

### Linux port 80 permissions

On Linux systems, binding to privileged ports below 1024 requires elevated privileges:
```sh
# Grant the binary capability to bind low ports:
sudo setcap cap_net_bind_service=+ep /path/to/yarmiplayservertv

# Or configure sysctl to allow non-root users to bind port 80:
sudo sysctl net.ipv4.ip_unprivileged_port_start=80
```

---

## Connecting from home (NAT loopback)

### Automatic local discovery

Many home routers do not support **NAT loopback** (hairpinning). This means that typing your public DuckDNS address while connected to your home Wi-Fi might fail because the router refuses to bounce packets back into the local network.

YarmiplayServerTV handles this seamlessly:
- **YarmiplayTV LAN Discovery:** YarmiplayTV clients broadcast a UDP discovery packet on your Syncplay port. YarmiplayServerTV replies with its local network IP.
- The player connects locally over your high-speed home network, yet verifies the connection against the public DuckDNS certificate so encryption remains completely intact.

### Direct IPv6 routing

If your internet provider offers IPv6, toggle on **Publish IPv6 too** on the Network page. Because IPv6 addresses are globally routable and do not use NAT, devices with IPv6 can connect using your domain name both at home and remotely without NAT loopback issues.

### LAN name for browsers

For browser viewing at home when your router lacks NAT loopback:
1. Create a second DuckDNS subdomain (for example `myfamilytv-lan`).
2. On the **Network** page, enter it under **LAN name for browsers**.
3. YarmiplayServerTV will point this second name directly at your computer's local IP address and add it to your TLS certificate.
4. Friends opening a browser at home can join without passing traffic through the outside internet.

---

## Watching in a web browser

Once your domain and HTTPS are configured, friends don't even need to install an app to watch together:

1. Open the **Network** page and copy the **Browser watch link** for your room.
2. Send the link to your friends (it looks like `https://tv.yarmiplay.com/watch/#join=myfamilytv.duckdns.org:8999/room`).
3. When opened in Chrome, Edge, Safari, or Firefox, the browser opens a secure WebSocket connection directly to your computer.
4. Video files can be streamed through your server's built-in file relay or from your Jellyfin server.

> **Next step:** Need help with libraries, device approvals, or user management? Head over to the [Setup Guide](../setup/).
