// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! The panel's IPv4 address on the local network, for the setup page's URL, read from the
//! kernel's tables in /proc/net: the interface of the default route (`route`) and the local
//! addresses (`fib_trie`). No socket and no packet: the bundle's SELinux domain may read
//! /proc/net (`proc_net`) but has no UDP socket, the usual way to ask the kernel for the address
//! a packet would leave from.

use std::net::Ipv4Addr;

/// The kernel's routing table (IPv4).
pub const ROUTE: &str = "/proc/net/route";
/// The kernel's FIB, with every local address.
pub const FIB_TRIE: &str = "/proc/net/fib_trie";
/// A route that is up (`RTF_UP`).
const RTF_UP: u32 = 0x1;

/// The panel's address now; None without a network.
pub fn read_lan_address() -> Option<Ipv4Addr> {
    let route = std::fs::read_to_string(ROUTE).ok()?;
    let fib_trie = std::fs::read_to_string(FIB_TRIE).ok()?;
    lan_address(&route, &fib_trie)
}

/// The local address on the subnet of the interface that has the default route (the lowest
/// metric); without a default route, a local address on the subnet of any interface but the
/// loopback. The texts are /proc/net/route and /proc/net/fib_trie.
pub fn lan_address(route: &str, fib_trie: &str) -> Option<Ipv4Addr> {
    let routes: Vec<Route> = route.lines().skip(1).filter_map(Route::parse).filter(|r| r.flags & RTF_UP != 0).collect();
    let locals = local_addresses(fib_trie);
    let default = routes.iter().filter(|r| r.mask == 0 && r.destination == 0).min_by_key(|r| r.metric);
    let on = |iface: &str| {
        routes.iter().filter(|r| r.iface == iface && r.mask != 0).find_map(|r| locals.iter().find(|a| (u32::from(**a) & r.mask) == (r.destination & r.mask))).copied()
    };
    match default {
        Some(d) => on(&d.iface),
        None => routes.iter().filter(|r| r.iface != "lo").find_map(|r| on(&r.iface)),
    }
}

/// A line of /proc/net/route: the addresses are the kernel's 32-bit words in network order,
/// printed as host-order hex.
struct Route {
    iface: String,
    destination: u32,
    flags: u32,
    metric: u32,
    mask: u32,
}

impl Route {
    fn parse(line: &str) -> Option<Route> {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 {
            return None;
        }
        let address = |hex: &str| u32::from_str_radix(hex, 16).ok().map(|w| u32::from(Ipv4Addr::from(w.to_ne_bytes())));
        Some(Route {
            iface: f[0].to_string(),
            destination: address(f[1])?,
            flags: u32::from_str_radix(f[3], 16).ok()?,
            metric: f[6].parse().ok()?,
            mask: address(f[7])?,
        })
    }
}

/// The host addresses (`/32 host LOCAL`) of /proc/net/fib_trie, without the loopback's.
fn local_addresses(fib_trie: &str) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut leaf: Option<Ipv4Addr> = None;
    for line in fib_trie.lines() {
        let line = line.trim();
        if let Some(address) = line.strip_prefix("|-- ") {
            leaf = address.parse().ok();
        } else if line.starts_with("/32 host LOCAL") {
            if let Some(a) = leaf.filter(|a| !a.is_loopback() && !out.contains(a)) {
                out.push(a);
            }
        } else if !line.starts_with('/') {
            leaf = None;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // /proc/net/route of the RB3 on Wi-Fi with Ethernet up too (little-endian: 0101A8C0 is
    // 192.168.1.1)
    const ROUTE_TEXT: &str = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
        eth0\t0000000A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n\
        wlan0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
        wlan0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";

    const FIB_TRIE_TEXT: &str = "Main:\n\
  +-- 0.0.0.0/0 3 0 5\n\
     |-- 0.0.0.0\n\
        /0 universe UNICAST\n\
     +-- 10.0.0.0/24 2 0 2\n\
        |-- 10.0.0.0\n\
           /24 link UNICAST\n\
        |-- 10.0.0.7\n\
           /32 host LOCAL\n\
     +-- 127.0.0.0/8 2 0 2\n\
        |-- 127.0.0.1\n\
           /32 host LOCAL\n\
     +-- 192.168.1.0/24 2 0 2\n\
        |-- 192.168.1.0\n\
           /24 link UNICAST\n\
        |-- 192.168.1.23\n\
           /32 host LOCAL\n\
        |-- 192.168.1.255\n\
           /32 link BROADCAST\n\
Local:\n\
  +-- 0.0.0.0/0 3 0 5\n\
        |-- 192.168.1.23\n\
           /32 host LOCAL\n";

    #[test]
    fn the_address_of_the_default_route() {
        assert_eq!(lan_address(ROUTE_TEXT, FIB_TRIE_TEXT), Some(Ipv4Addr::new(192, 168, 1, 23)));
    }

    #[test]
    fn without_a_default_route_any_interface() {
        let no_default: String = ROUTE_TEXT.lines().filter(|l| !l.contains("0101A8C0")).collect::<Vec<_>>().join("\n");
        assert_eq!(lan_address(&no_default, FIB_TRIE_TEXT), Some(Ipv4Addr::new(10, 0, 0, 7)));
    }

    #[test]
    fn no_network_no_address() {
        let header = ROUTE_TEXT.lines().next().unwrap();
        assert_eq!(lan_address(header, FIB_TRIE_TEXT), None);
        assert_eq!(lan_address(ROUTE_TEXT, "Main:\n  +-- 127.0.0.0/8 2 0 2\n     |-- 127.0.0.1\n        /32 host LOCAL\n"), None);
        // a route that is down does not count
        let down = ROUTE_TEXT.replace("wlan0\t00000000\t0101A8C0\t0003", "wlan0\t00000000\t0101A8C0\t0002");
        assert_eq!(lan_address(&down, FIB_TRIE_TEXT), Some(Ipv4Addr::new(10, 0, 0, 7)));
        assert_eq!(lan_address("", ""), None);
        assert_eq!(lan_address("garbage\nx y z\n", "|-- nonsense\n/32 host LOCAL\n"), None);
    }
}
