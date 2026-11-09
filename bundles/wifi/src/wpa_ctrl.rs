//! wpa_supplicant's control interface (hostap's wpa_ctrl protocol), the part this bundle
//! needs: a request gets a reply on a datagram socket; a second, attached socket receives
//! the unsolicited events (`<3>CTRL-EVENT-...`). The supplicant serves its global interface
//! on an abstract unix socket (`-g@abstract:<name>`, device/rb3 net/net_bringup.sh): no
//! file, no DAC, SELinux alone decides who may talk to it (bundles/wifi/README.md). On the
//! global interface every request names the interface: `IFNAME=wlan0 SCAN`.

use std::io;
#[cfg(target_os = "android")]
use std::os::android::net::SocketAddrExt;
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::SocketAddr;
use std::time::Duration;
use tokio::net::UnixDatagram;
use tokio::time::timeout;

/// The global control interface, as net_bringup.sh starts the supplicant.
pub const GLOBAL_CTRL: &str = "@abstract:wpa_everyware";
/// The interface the commands go to.
pub const IFACE: &str = "wlan0";
/// A reply comes within this, or the supplicant is gone.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// The supplicant's replies and events fit in this (SCAN_RESULTS of 32 networks ~ 3 KB).
const BUF: usize = 16 * 1024;
/// sizeof(sockaddr_un.sun_path) on Linux.
const SUN_PATH_LEN: usize = 108;

/// One connection to the control interface: requests, or events after `attach`.
pub struct Ctrl {
    socket: UnixDatagram,
    /// `IFNAME=wlan0 ` before every request on the global interface.
    prefix: String,
}

impl Ctrl {
    /// Connects to the supplicant's control socket `path` (`@abstract:<name>` or a file) for
    /// the interface `iface`; `tag` makes this client's own (abstract) socket name unique.
    pub fn open(path: &str, iface: &str, tag: &str) -> io::Result<Ctrl> {
        let local = SocketAddr::from_abstract_name(format!("everyware_wifi_{}_{tag}", std::process::id()).as_bytes())?;
        let socket = std::os::unix::net::UnixDatagram::bind_addr(&local)?;
        let remote = match path.strip_prefix("@abstract:") {
            // The supplicant binds the whole sun_path (ctrl_iface_unix.c: sizeof(addr)), so
            // its abstract name is the text padded with NULs to the field's length; an
            // abstract name is exactly as long as it was bound, a shorter one is another
            // socket (/proc/net/unix shows it as @wpa_everyware@@@...).
            Some(name) => {
                let mut padded = name.as_bytes().to_vec();
                padded.resize(SUN_PATH_LEN - 1, 0);
                SocketAddr::from_abstract_name(&padded)?
            }
            None => SocketAddr::from_pathname(path)?,
        };
        socket.connect_addr(&remote)?;
        socket.set_nonblocking(true)?;
        let prefix = if path.starts_with("@abstract:") || !path.contains(iface) { format!("IFNAME={iface} ") } else { String::new() };
        Ok(Ctrl { socket: UnixDatagram::from_std(socket)?, prefix })
    }

    /// Sends a request for the interface and returns the reply (without the trailing newline).
    pub async fn request(&self, command: &str) -> io::Result<String> {
        self.request_raw(&format!("{}{command}", self.prefix)).await
    }

    /// Sends a request as it is: the global interface takes ATTACH / DETACH itself, before
    /// any IFNAME= routing (ctrl_iface_unix.c, wpa_supplicant_global_ctrl_iface_receive).
    async fn request_raw(&self, command: &str) -> io::Result<String> {
        self.socket.send(command.as_bytes()).await?;
        let mut buf = vec![0u8; BUF];
        loop {
            let n = timeout(REPLY_TIMEOUT, self.socket.recv(&mut buf))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, format!("no reply to {command}")))??;
            let reply = String::from_utf8_lossy(&buf[..n]).trim_end().to_string();
            // an event that slipped in on the request socket is not the reply
            if reply.starts_with('<') {
                continue;
            }
            return Ok(reply);
        }
    }

    /// A request that must be answered with OK.
    pub async fn ok(&self, command: &str) -> io::Result<()> {
        match self.request(command).await?.as_str() {
            "OK" => Ok(()),
            other => Err(io::Error::other(format!("{command}: {other}"))),
        }
    }

    /// Attaches this connection to the supplicant's events (on the global interface: the
    /// events of every interface, each prefixed with its IFNAME=).
    pub async fn attach(&self) -> io::Result<()> {
        match self.request_raw("ATTACH").await?.as_str() {
            "OK" => Ok(()),
            other => Err(io::Error::other(format!("ATTACH: {other}"))),
        }
    }

    /// The next event, without its `IFNAME=` and `<level>` prefixes.
    pub async fn event(&self) -> io::Result<String> {
        let mut buf = vec![0u8; BUF];
        loop {
            let n = self.socket.recv(&mut buf).await?;
            let text = String::from_utf8_lossy(&buf[..n]).trim_end().to_string();
            let text = text.strip_prefix("IFNAME=").and_then(|t| t.split_once(' ')).map_or(text.as_str(), |(_, rest)| rest);
            if let Some(rest) = text.strip_prefix('<').and_then(|t| t.split_once('>')).map(|(_, rest)| rest) {
                return Ok(rest.to_string());
            }
        }
    }
}

/// A line of SCAN_RESULTS: bssid, frequency, signal level, flags, ssid (tab-separated).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanEntry {
    pub ssid: String,
    pub signal_dbm: i32,
    pub flags: String,
}

/// Parses SCAN_RESULTS: the header line, then one line per BSS; hidden networks (no SSID)
/// are left out.
pub fn parse_scan_results(reply: &str) -> Vec<ScanEntry> {
    reply
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 5 || fields[4].is_empty() {
                return None;
            }
            Some(ScanEntry { ssid: fields[4].to_string(), signal_dbm: fields[2].parse().ok()?, flags: fields[3].to_string() })
        })
        .collect()
}

/// Parses LIST_NETWORKS: (id, ssid, current) per saved network.
pub fn parse_networks(reply: &str) -> Vec<(u32, String, bool)> {
    reply
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            Some((fields.first()?.parse().ok()?, fields.get(1)?.to_string(), fields.get(3).is_some_and(|f| f.contains("CURRENT"))))
        })
        .collect()
}

/// A value of STATUS (`key=value` lines).
pub fn status_value<'a>(reply: &'a str, key: &str) -> Option<&'a str> {
    reply.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// Bars from the signal level, as Android's WifiManager levels (5 levels, 4 bars here).
pub fn bars(signal_dbm: i32) -> u32 {
    match signal_dbm {
        d if d >= -55 => 4,
        d if d >= -66 => 3,
        d if d >= -77 => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_results_and_networks_parse() {
        let scan = "bssid / frequency / signal level / flags / ssid\n30:de:4b:39:bc:8e\t2422\t-48\t[WPA2-PSK-CCMP][ESS]\tfaraway\naa:bb\t5180\t-70\t[WPA2-PSK-CCMP][SAE][ESS]\tNachbar-5G\ncc:dd\t2437\t-80\t[ESS]\t\n";
        let entries = parse_scan_results(scan);
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].ssid.as_str(), entries[0].signal_dbm, bars(entries[0].signal_dbm)), ("faraway", -48, 4));
        assert_eq!(bars(entries[1].signal_dbm), 2);
        let nets = parse_networks("network id / ssid / bssid / flags\n0\tfaraway\tany\t[CURRENT]\n3\tGuest\tany\t\n");
        assert_eq!(nets, vec![(0, "faraway".to_string(), true), (3, "Guest".to_string(), false)]);
        assert_eq!(status_value("bssid=x\nssid=faraway\nwpa_state=COMPLETED", "wpa_state"), Some("COMPLETED"));
    }
}
