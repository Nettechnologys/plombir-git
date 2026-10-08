//! The one answer to "which address is this request from?".
//!
//! Every consumer of a client address — the per-IP limiters, the audit log,
//! the login log, the per-source share of the password limiter, the git
//! session limiter — used to read it its own way, and most of them read
//! whatever the client wrote into `X-Forwarded-For` (card_5d48237b16b0). The
//! limiter, the one that did consult `trusted_proxies`, took the *left* entry
//! of the chain, which a proxy that appends (nginx's
//! `$proxy_add_x_forwarded_for`) leaves under the client's control
//! (card_c2f0454ceb89).
//!
//! The rule now lives here and nowhere else:
//!
//! - A request whose TCP peer is not a trusted proxy is from that peer. Its
//!   forwarding headers are the client's own words and are ignored.
//! - Behind trusted proxies, the `X-Forwarded-For` chain is walked from the
//!   right — the entries the proxies appended — and the first address that is
//!   not itself a trusted proxy is the client. Anything to the left of it was
//!   written by the client and is never read.
//! - An entry that does not parse ends the walk: the client is the nearest
//!   proxy we do trust, never the garbage.
//!
//! [`resolve_client_ip_middleware`] runs this once per request, ahead of every
//! other layer, and publishes the result twice: as the [`ClientIp`] request
//! extension for typed consumers, and as [`CLIENT_IP_HEADER`] for the code
//! below `rg-http` that only ever sees a `HeaderMap` (`rg_core::audit::record`
//! and its forty-odd call sites). The header is stripped from every incoming
//! request first, so a value under that name is always the server's.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

pub use rg_core::audit::CLIENT_IP_HEADER;

/// The resolved client address of a request, as a request extension.
///
/// Absent when the server cannot know the address at all — a request that did
/// not arrive over a socket (the in-process test harness).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// Resolves the client address against the operator's `trusted_proxies`.
#[derive(Debug, Clone, Default)]
pub struct ClientIpResolver {
    trusted_proxies: Arc<[IpAddr]>,
}

impl ClientIpResolver {
    /// A resolver that believes forwarding headers only from these peers.
    pub fn new(trusted_proxies: Vec<IpAddr>) -> Self {
        let trusted: Vec<IpAddr> = trusted_proxies
            .into_iter()
            .map(|ip| ip.to_canonical())
            .collect();
        Self {
            trusted_proxies: trusted.into(),
        }
    }

    fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.contains(&ip)
    }

    /// The client address of a request that arrived from `peer`.
    pub fn resolve(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        let peer = peer.to_canonical();
        if !self.is_trusted(peer) {
            return peer;
        }

        let hops = forwarded_for_hops(headers);
        if hops.is_empty() {
            // No chain: a proxy that sets `X-Real-IP` overwrites it, so its
            // value is the proxy's word — but only from a trusted peer, which
            // is where we are.
            return headers
                .get("x-real-ip")
                .and_then(|value| value.to_str().ok())
                .and_then(parse_hop)
                .map(|ip| ip.to_canonical())
                .unwrap_or(peer);
        }

        let mut nearest_trusted = peer;
        for hop in hops.iter().rev() {
            match hop {
                Some(ip) if self.is_trusted(*ip) => nearest_trusted = *ip,
                Some(ip) => return *ip,
                // A hop we cannot read was not written by a proxy we trust to
                // write addresses; stop at the last one we do.
                None => return nearest_trusted,
            }
        }
        // Every hop is one of our own proxies: the request started there.
        nearest_trusted
    }
}

/// Every `X-Forwarded-For` entry, left to right across all instances of the
/// header, each parsed (`None` when it is not an address).
fn forwarded_for_hops(headers: &HeaderMap) -> Vec<Option<IpAddr>> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .flat_map(|value| match value.to_str() {
            Ok(text) => text
                .split(',')
                .map(|hop| parse_hop(hop).map(|ip| ip.to_canonical()))
                .collect::<Vec<_>>(),
            // A header that is not even text is one unreadable hop.
            Err(_) => vec![None],
        })
        .collect()
}

/// One forwarding entry: a bare address, or one with a port (`1.2.3.4:80`,
/// `[2001:db8::1]:443`), which some proxies write.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    hop.parse::<IpAddr>()
        .ok()
        .or_else(|| hop.parse::<SocketAddr>().ok().map(|addr| addr.ip()))
}

/// The address an abuse budget is kept for.
///
/// IPv4 addresses stand alone. An IPv6 address is aggregated to its /64: that
/// is what one subscriber is handed, and keying the full address let one host
/// rotate through 2^64 of them — a fresh budget every request, and enough
/// distinct keys to fill any limiter's map (card_c2f0454ceb89). An
/// IPv4-mapped address (`::ffff:a.b.c.d`, what a dual-stack listener reports)
/// is its IPv4 address: aggregating it as IPv6 would put every IPv4 client on
/// the internet into the one bucket `::/64`.
pub fn budget_key(ip: IpAddr) -> String {
    match rg_core::net::abuse_source(ip) {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(prefix) => format!("{prefix}/64"),
    }
}

/// The resolved client address [`resolve_client_ip_middleware`] left in
/// `headers`, for code that holds only the headers.
pub fn from_headers(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get(CLIENT_IP_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
}

/// Resolve the client address once, before every other layer sees the
/// request. See the module documentation.
pub async fn resolve_client_ip_middleware(
    State(resolver): State<ClientIpResolver>,
    mut request: Request,
    next: Next,
) -> Response {
    request.headers_mut().remove(CLIENT_IP_HEADER);
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    if let Some(peer) = peer {
        let ip = resolver.resolve(peer, request.headers());
        request.extensions_mut().insert(ClientIp(ip));
        if let Ok(value) = HeaderValue::from_str(&ip.to_string()) {
            request.headers_mut().insert(CLIENT_IP_HEADER, value);
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY: &str = "10.0.0.2";

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, value.parse().unwrap());
        }
        headers
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn behind_proxy() -> ClientIpResolver {
        ClientIpResolver::new(vec![ip(PROXY), ip("10.0.0.3")])
    }

    #[test]
    fn an_untrusted_peer_is_the_client_whatever_it_writes() {
        let resolver = behind_proxy();
        let forged = headers(&[("x-forwarded-for", "1.2.3.4"), ("x-real-ip", "5.6.7.8")]);
        assert_eq!(
            resolver.resolve(ip("198.51.100.9"), &forged),
            ip("198.51.100.9")
        );
        // No proxy configured at all: the same.
        assert_eq!(
            ClientIpResolver::default().resolve(ip("198.51.100.9"), &forged),
            ip("198.51.100.9")
        );
    }

    /// nginx `$proxy_add_x_forwarded_for` appends the peer it saw to whatever
    /// the client sent: the client's own entry is on the left, the real one on
    /// the right. card_c2f0454ceb89.
    #[test]
    fn behind_an_appending_proxy_the_rightmost_untrusted_entry_wins() {
        let resolver = behind_proxy();
        let chain = headers(&[("x-forwarded-for", "1.2.3.4, 203.0.113.7")]);
        assert_eq!(resolver.resolve(ip(PROXY), &chain), ip("203.0.113.7"));
    }

    #[test]
    fn a_chain_of_trusted_proxies_is_walked_past() {
        let resolver = behind_proxy();
        let chain = headers(&[("x-forwarded-for", "9.9.9.9, 203.0.113.7, 10.0.0.3")]);
        assert_eq!(resolver.resolve(ip(PROXY), &chain), ip("203.0.113.7"));

        // Split across two header lines, in order.
        let split = headers(&[
            ("x-forwarded-for", "9.9.9.9, 203.0.113.7"),
            ("x-forwarded-for", "10.0.0.3"),
        ]);
        assert_eq!(resolver.resolve(ip(PROXY), &split), ip("203.0.113.7"));
    }

    #[test]
    fn an_unreadable_hop_stops_at_the_nearest_trusted_proxy() {
        let resolver = behind_proxy();
        let chain = headers(&[("x-forwarded-for", "203.0.113.7, not-an-ip, 10.0.0.3")]);
        assert_eq!(resolver.resolve(ip(PROXY), &chain), ip("10.0.0.3"));
    }

    #[test]
    fn hops_with_ports_and_mapped_addresses_are_addresses() {
        let resolver = behind_proxy();
        let chain = headers(&[("x-forwarded-for", "[2001:db8::1]:443")]);
        assert_eq!(resolver.resolve(ip(PROXY), &chain), ip("2001:db8::1"));
        let chain = headers(&[("x-forwarded-for", "203.0.113.7:5000")]);
        assert_eq!(resolver.resolve(ip(PROXY), &chain), ip("203.0.113.7"));
        // A dual-stack listener reports the proxy as `::ffff:10.0.0.2`.
        let chain = headers(&[("x-forwarded-for", "::ffff:203.0.113.7")]);
        assert_eq!(
            resolver.resolve(ip("::ffff:10.0.0.2"), &chain),
            ip("203.0.113.7")
        );
    }

    #[test]
    fn x_real_ip_counts_only_without_a_chain() {
        let resolver = behind_proxy();
        let only = headers(&[("x-real-ip", "203.0.113.7")]);
        assert_eq!(resolver.resolve(ip(PROXY), &only), ip("203.0.113.7"));
        let both = headers(&[
            ("x-forwarded-for", "198.51.100.1"),
            ("x-real-ip", "203.0.113.7"),
        ]);
        assert_eq!(resolver.resolve(ip(PROXY), &both), ip("198.51.100.1"));
        assert_eq!(resolver.resolve(ip(PROXY), &HeaderMap::new()), ip(PROXY));
    }

    #[test]
    fn ipv6_budgets_are_per_64_and_mapped_ipv4_is_ipv4() {
        assert_eq!(
            budget_key(ip("2001:db8:1:2:aaaa::1")),
            budget_key(ip("2001:db8:1:2:bbbb::2"))
        );
        assert_ne!(
            budget_key(ip("2001:db8:1:2::1")),
            budget_key(ip("2001:db8:1:3::1"))
        );
        assert_eq!(budget_key(ip("2001:db8:1:2::1")), "2001:db8:1:2::/64");
        assert_eq!(budget_key(ip("::ffff:203.0.113.7")), "203.0.113.7");
        assert_ne!(
            budget_key(ip("::ffff:203.0.113.7")),
            budget_key(ip("::ffff:198.51.100.1")),
            "IPv4 clients behind a dual-stack listener must not share a bucket"
        );
    }

    #[tokio::test]
    async fn the_middleware_overwrites_a_client_supplied_header() {
        use axum::body::Body;
        use axum::routing::get;
        use axum::Router;
        use tower::ServiceExt;

        async fn echo(headers: HeaderMap) -> String {
            from_headers(&headers)
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "none".to_string())
        }
        let app: Router =
            Router::new()
                .route("/", get(echo))
                .layer(axum::middleware::from_fn_with_state(
                    ClientIpResolver::default(),
                    resolve_client_ip_middleware,
                ));
        let request = |connect: bool| {
            let mut request = axum::http::Request::builder()
                .uri("/")
                .header(CLIENT_IP_HEADER, "1.2.3.4")
                .header("x-forwarded-for", "5.6.7.8")
                .body(Body::empty())
                .unwrap();
            if connect {
                request.extensions_mut().insert(ConnectInfo(
                    "198.51.100.9:4000".parse::<SocketAddr>().unwrap(),
                ));
            }
            request
        };
        let body = |response: Response| async move {
            let bytes = axum::body::to_bytes(response.into_body(), 64)
                .await
                .unwrap();
            String::from_utf8(bytes.to_vec()).unwrap()
        };

        let over_a_socket = app.clone().oneshot(request(true)).await.unwrap();
        assert_eq!(body(over_a_socket).await, "198.51.100.9");
        // No socket, no address — and never the one the client wrote.
        let in_process = app.oneshot(request(false)).await.unwrap();
        assert_eq!(body(in_process).await, "none");
    }
}
