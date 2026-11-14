//! Print the read-only backend's view of this machine as JSON once and exit.
//!
//! Reads `/sys/class/net`, `/proc/net/route`, `/etc/resolv.conf` and `getifaddrs`; sends no
//! packet and changes nothing.
//!
//!     cargo run -p ashd-svc-network --example dump

use ashd_svc_network::{NetworkState, Paths, fmt, sample_once};
use serde_json::json;

fn state_json(s: &NetworkState) -> serde_json::Value {
    json!({
        "backend": format!("{:?}", s.backend),
        "can_configure": s.can_configure,
        "primary": s.primary,
        "tile": { "title": s.tile_title(), "subtitle": s.tile_subtitle() },
        "dns": s.dns,
        "vpns": s.vpns,
        "interfaces": s.interfaces.iter().map(|i| json!({
            "name": i.name,
            "kind": i.kind,
            "operstate": i.operstate,
            "carrier": i.carrier,
            "link": i.link_text(),
            "mac": i.mac,
            "ipv4": i.ipv4.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "ipv6": i.ipv6.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "gateway": i.gateway,
            "is_default": i.is_default,
            "rx_bytes": i.rx_bytes,
            "tx_bytes": i.tx_bytes,
            "link_up": i.link_up_secs.map(fmt::uptime),
        })).collect::<Vec<_>>(),
    })
}

fn main() {
    let state = sample_once(Paths::system());
    println!("{}", serde_json::to_string_pretty(&state_json(&state)).expect("serialize"));
}
