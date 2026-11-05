//! Where a request came from: the client IP (through trusted proxies), whether it counts as
//! remote, and whether it arrived over HTTPS.

use super::Shared;
use crate::auth::perm::Origin;
use crate::config::ServerConfig;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::Response;
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientInfo {
    pub ip: IpAddr,
    pub origin: Origin,
    pub https: bool,
}

impl ClientInfo {
    /// Forwarding headers count only when the TCP peer is a trusted proxy. A request is
    /// remote when it came through a remote proxy, carries Cloudflare headers (whoever set
    /// them: spoofing only makes a request more remote), or its IP is outside the local
    /// networks.
    pub fn from_parts(cfg: &ServerConfig, peer: IpAddr, headers: &HeaderMap) -> ClientInfo {
        let peer = peer.to_canonical();
        let via_proxy = cfg.trusted_proxies.contains(&peer);
        let cloudflare = headers.contains_key("cf-connecting-ip") || headers.contains_key("cf-ray");
        let mut ip = peer;
        let mut https = false;
        if via_proxy {
            if let Some(cf) = header_ip(headers, "cf-connecting-ip") {
                ip = cf;
            } else if let Some(fwd) = forwarded_for(headers, &cfg.trusted_proxies) {
                ip = fwd;
            }
            https = headers
                .get("x-forwarded-proto")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("https"));
        }
        let local = cfg.local_networks.iter().any(|n| n.contains(&ip));
        let remote = cfg.remote_proxies.contains(&peer) || cloudflare || !local;
        ClientInfo {
            ip,
            origin: if remote {
                Origin::Remote
            } else {
                Origin::Local
            },
            https,
        }
    }
}

fn header_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_canonical())
}

/// The right-most X-Forwarded-For address that isn't one of our own proxies.
fn forwarded_for(headers: &HeaderMap, trusted: &[IpAddr]) -> Option<IpAddr> {
    let all: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
        .collect();
    all.iter().rev().find(|ip| !trusted.contains(ip)).copied()
}

pub async fn layer(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let info = ClientInfo::from_parts(&st.cfg.server, peer.ip(), req.headers());
    req.extensions_mut().insert(info);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn server() -> ServerConfig {
        Config::from_toml_str(
            "[server]\npublic_url = \"https://x.org\"\n\
             trusted_proxies = [\"192.168.168.136\", \"192.168.168.211\"]\n\
             remote_proxies = [\"192.168.168.136\"]\n",
        )
        .unwrap()
        .server
    }

    fn hdrs(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, v.parse().unwrap());
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn tunnel_requests_are_remote_with_the_real_ip() {
        let c = ClientInfo::from_parts(
            &server(),
            ip("192.168.168.136"),
            &hdrs(&[
                ("cf-connecting-ip", "203.0.113.7"),
                ("x-forwarded-proto", "https"),
            ]),
        );
        assert_eq!(
            c,
            ClientInfo {
                ip: ip("203.0.113.7"),
                origin: Origin::Remote,
                https: true
            }
        );
    }

    #[test]
    fn lan_through_npm_is_local() {
        let c = ClientInfo::from_parts(
            &server(),
            ip("192.168.168.211"),
            &hdrs(&[
                ("x-forwarded-for", "192.168.168.115"),
                ("x-forwarded-proto", "https"),
            ]),
        );
        assert_eq!(
            c,
            ClientInfo {
                ip: ip("192.168.168.115"),
                origin: Origin::Local,
                https: true
            }
        );
    }

    #[test]
    fn spoofed_headers_from_a_lan_client_are_not_trusted_but_make_it_remote() {
        let c = ClientInfo::from_parts(
            &server(),
            ip("192.168.168.50"),
            &hdrs(&[
                ("cf-connecting-ip", "1.2.3.4"),
                ("x-forwarded-proto", "https"),
            ]),
        );
        assert_eq!(c.ip, ip("192.168.168.50"));
        assert_eq!(c.origin, Origin::Remote);
        assert!(!c.https);
        let plain = ClientInfo::from_parts(
            &server(),
            ip("192.168.168.50"),
            &hdrs(&[("x-forwarded-for", "1.2.3.4")]),
        );
        assert_eq!(
            (plain.ip, plain.origin),
            (ip("192.168.168.50"), Origin::Local)
        );
    }

    #[test]
    fn direct_public_and_tailscale_addresses() {
        assert_eq!(
            ClientInfo::from_parts(&server(), ip("198.51.100.9"), &HeaderMap::new()).origin,
            Origin::Remote
        );
        assert_eq!(
            ClientInfo::from_parts(&server(), ip("100.96.12.70"), &HeaderMap::new()).origin,
            Origin::Local
        );
        assert_eq!(
            ClientInfo::from_parts(&server(), ip("::ffff:192.168.168.20"), &HeaderMap::new()).ip,
            ip("192.168.168.20")
        );
    }

    #[test]
    fn rightmost_untrusted_forwarded_address_wins() {
        let c = ClientInfo::from_parts(
            &server(),
            ip("192.168.168.211"),
            &hdrs(&[(
                "x-forwarded-for",
                "6.6.6.6, 192.168.168.30, 192.168.168.136",
            )]),
        );
        assert_eq!(c.ip, ip("192.168.168.30"));
    }
}
