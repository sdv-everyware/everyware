//! The network the controller talks on: one interface (the house's Wi-Fi), its addresses, and
//! the two sockets: Matter's (an ephemeral port: the controller only initiates) and mDNS's
//! (port 5353 with the multicast groups joined, for rs-matter's built-in responder, which
//! resolves and browses the devices).

use rs_matter::transport::network::mdns::{MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR};
use rs_matter::transport::network::{Ipv4Addr, Ipv6Addr};
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{SocketAddr, UdpSocket};

/// An interface with what Matter needs of it: an IPv6 link-local address at least (Matter runs
/// over IPv6), and IPv4 for the mDNS answers of devices that only send those.
#[derive(Clone, Debug, PartialEq)]
pub struct Interface {
    pub name: String,
    pub index: u32,
    pub ipv4: Ipv4Addr,
    pub ipv6: Vec<Ipv6Addr>,
}

/// The interface `name` once it has an IPv6 link-local address (at boot it has none until the
/// Wi-Fi is up), or why not yet. Its IPv6 addresses and index come from /proc/net/if_inet6, the
/// kernel's list of every IPv6 address (getifaddrs over netlink missed the link-local one in the
/// bundle), its IPv4 address from getifaddrs.
pub fn interface(name: &str) -> Result<Interface, String> {
    let table = std::fs::read_to_string("/proc/net/if_inet6").map_err(|e| format!("/proc/net/if_inet6: {e}"))?;
    let mut index = None;
    let mut ipv6: Vec<Ipv6Addr> = Vec::new();
    for (addr, if_index, if_name) in parse_if_inet6(&table) {
        if if_name == name {
            index = Some(if_index);
            if !addr.is_loopback() {
                ipv6.push(addr);
            }
        }
    }
    let Some(index) = index else { return Err(format!("{name} has no IPv6 address yet")) };
    // the link-local one first: the one every device on the link reaches
    ipv6.sort_by_key(|ip| !ip.is_unicast_link_local());
    if !ipv6.first().is_some_and(|ip| ip.is_unicast_link_local()) {
        return Err(format!("{name} has no IPv6 link-local address yet ({ipv6:?})"));
    }
    let ipv4 = if_addrs::get_if_addrs()
        .ok()
        .and_then(|all| {
            all.into_iter().find_map(|ia| match ia.addr {
                if_addrs::IfAddr::V4(v4) if ia.name == name => Some(v4.ip),
                _ => None,
            })
        })
        .unwrap_or(Ipv4Addr::UNSPECIFIED);
    Ok(Interface { name: name.to_string(), index, ipv4, ipv6 })
}

/// The lines of /proc/net/if_inet6: "<address, 32 hex digits> <index hex> <prefix> <scope>
/// <flags> <name>"; malformed lines are skipped.
fn parse_if_inet6(table: &str) -> Vec<(Ipv6Addr, u32, String)> {
    table
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 6 || f[0].len() != 32 {
                return None;
            }
            let addr = Ipv6Addr::from(u128::from_str_radix(f[0], 16).ok()?);
            Some((addr, u32::from_str_radix(f[1], 16).ok()?, f[5].to_string()))
        })
        .collect()
}

/// The socket Matter's transport sends and receives on: both families, an ephemeral port.
pub fn matter_socket() -> io::Result<async_io::Async<UdpSocket>> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_only_v6(false)?;
    socket.bind(&SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)).into())?;
    async_io::Async::new_nonblocking(UdpSocket::from(socket))
}

/// The mDNS socket on `interface`: port 5353 shared with any other responder, both groups.
pub fn mdns_socket(interface: &Interface) -> io::Result<async_io::Async<UdpSocket>> {
    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_only_v6(false)?;
    socket.bind(&MDNS_SOCKET_DEFAULT_BIND_ADDR.into())?;
    let socket = UdpSocket::from(socket);
    socket.join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, interface.index)?;
    if !interface.ipv4.is_unspecified() {
        socket.join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &interface.ipv4)?;
    }
    async_io::Async::new_nonblocking(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_inet6_lines() {
        let table = "fe80000000000000e0c67efffebc4636 10 40 20 80    wlan0\n00000000000000000000000000000001 01 80 10 80       lo\nbroken\n";
        let rows = parse_if_inet6(table);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("fe80::e0c6:7eff:febc:4636".parse().unwrap(), 0x10, "wlan0".to_string()));
        assert!(rows[0].0.is_unicast_link_local());
    }
}
