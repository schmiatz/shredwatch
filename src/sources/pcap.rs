use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use anyhow::Result;

use crate::config::PcapConfig;
use crate::registry::{ShredEvent, SourceId};

pub async fn run(
    config: PcapConfig,
    source_id: SourceId,
    tx: mpsc::UnboundedSender<ShredEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        tokio::task::spawn_blocking(move || {
            if let Err(e) = linux::capture_loop(&config, source_id, &tx, &cancel) {
                tracing::error!("Raw packet capture error: {:#}", e);
            }
        });
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, source_id, tx, cancel);
        anyhow::bail!("Raw packet capture (AF_PACKET) is only supported on Linux")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::CString;
    use std::time::Instant;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use tracing::{info, warn};

    use crate::config::PcapConfig;
    use crate::registry::{ShredEvent, SourceId};
    use crate::shred::parse_shred_key;

    /// Extract the UDP payload from a raw Ethernet frame.
    /// Returns None if not IPv4/UDP or fragmented.
    fn extract_udp_payload(buf: &[u8]) -> Option<&[u8]> {
        if buf.len() < 42 { return None; }
        // Ethertype must be IPv4 (0x0800)
        if u16::from_be_bytes([buf[12], buf[13]]) != 0x0800 { return None; }
        let ip_start = 14;
        let ihl = ((buf[ip_start] & 0x0f) * 4) as usize;
        if ihl < 20 || buf.len() < ip_start + ihl + 8 { return None; }
        // Protocol must be UDP (17)
        if buf[ip_start + 9] != 17 { return None; }
        // Reject fragmented packets
        if u16::from_be_bytes([buf[ip_start + 6], buf[ip_start + 7]]) & 0x1fff != 0 {
            return None;
        }
        let udp_start = ip_start + ihl;
        let payload_start = udp_start + 8;
        let udp_len = u16::from_be_bytes([buf[udp_start + 4], buf[udp_start + 5]]) as usize;
        let payload_len = udp_len.saturating_sub(8);
        if payload_start + payload_len > buf.len() { return None; }
        Some(&buf[payload_start..payload_start + payload_len])
    }

    pub fn capture_loop(
        config: &PcapConfig,
        source_id: SourceId,
        tx: &mpsc::UnboundedSender<ShredEvent>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        // AF_PACKET SOCK_RAW delivers complete Ethernet frames.
        // Requires CAP_NET_RAW capability (or root).
        let fd = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW,
                (libc::ETH_P_IP as u16).to_be() as libc::c_int,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EPERM) {
                anyhow::bail!(
                    "Permission denied: AF_PACKET requires CAP_NET_RAW. \
                     Grant it with: sudo setcap cap_net_raw=eip ./shredwatch"
                );
            }
            return Err(err.into());
        }

        // Classic BPF filter: udp dst port <port>
        // All offsets are from the start of the Ethernet frame.
        //
        //  [0]  ldh  [12]          — ethertype
        //  [1]  jeq  #0x0800       — IPv4?          skip → drop
        //  [2]  ldb  [23]          — IP protocol
        //  [3]  jeq  #17           — UDP?            skip → drop
        //  [4]  ldh  [20]          — IP flags+frag offset
        //  [5]  jset #0x1fff       — fragmented?     match → drop
        //  [6]  ldxb 4*([14]&0xf)  — x = IP header length
        //  [7]  ldh  [x+16]        — UDP dst port (eth=14, udp_dport_off=2 → x+16)
        //  [8]  jeq  #PORT         — our port?       skip → drop
        //  [9]  ret  #0xffff       — accept
        //  [10] ret  #0            — drop
        let port = config.port as u32;
        let filter: [libc::sock_filter; 11] = [
            libc::sock_filter { code: 0x28, jt: 0, jf: 0,  k: 12     },
            libc::sock_filter { code: 0x15, jt: 0, jf: 8,  k: 0x0800 },
            libc::sock_filter { code: 0x30, jt: 0, jf: 0,  k: 23     },
            libc::sock_filter { code: 0x15, jt: 0, jf: 6,  k: 0x11   },
            libc::sock_filter { code: 0x28, jt: 0, jf: 0,  k: 20     },
            libc::sock_filter { code: 0x45, jt: 4, jf: 0,  k: 0x1fff },
            libc::sock_filter { code: 0xb1, jt: 0, jf: 0,  k: 14     },
            libc::sock_filter { code: 0x48, jt: 0, jf: 0,  k: 16     },
            libc::sock_filter { code: 0x15, jt: 0, jf: 1,  k: port   },
            libc::sock_filter { code: 0x06, jt: 0, jf: 0,  k: 0xffff },
            libc::sock_filter { code: 0x06, jt: 0, jf: 0,  k: 0      },
        ];
        let prog = libc::sock_fprog {
            len: filter.len() as u16,
            filter: filter.as_ptr() as *mut libc::sock_filter,
        };
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ATTACH_FILTER,
                &prog as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::sock_fprog>() as libc::socklen_t,
            )
        } < 0 {
            unsafe { libc::close(fd); }
            return Err(std::io::Error::last_os_error().into());
        }

        // Optionally bind to a specific interface; omitting captures all interfaces.
        if !config.interface.is_empty() {
            let ifindex = unsafe {
                let name = CString::new(config.interface.as_str())?;
                libc::if_nametoindex(name.as_ptr())
            };
            if ifindex == 0 {
                unsafe { libc::close(fd); }
                anyhow::bail!("Interface '{}' not found", config.interface);
            }
            let sll = libc::sockaddr_ll {
                sll_family:   libc::AF_PACKET as u16,
                sll_protocol: (libc::ETH_P_IP as u16).to_be(),
                sll_ifindex:  ifindex as i32,
                sll_hatype:   0,
                sll_pkttype:  0,
                sll_halen:    0,
                sll_addr:     [0; 8],
            };
            if unsafe {
                libc::bind(
                    fd,
                    &sll as *const libc::sockaddr_ll as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
                )
            } < 0 {
                unsafe { libc::close(fd); }
                return Err(std::io::Error::last_os_error().into());
            }
        }

        // Receive buffer size (optional)
        if config.recv_buf_size > 0 {
            let size = config.recv_buf_size as libc::c_int;
            unsafe {
                libc::setsockopt(
                    fd, libc::SOL_SOCKET, libc::SO_RCVBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        // 100ms receive timeout so cancellation is checked regularly
        let tv = libc::timeval { tv_sec: 0, tv_usec: 100_000 };
        unsafe {
            libc::setsockopt(
                fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO,
                &tv as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }

        let iface = if config.interface.is_empty() { "all interfaces" } else { &config.interface };
        info!("Raw packet capture started (port={}, iface={})", config.port, iface);

        let mut buf = vec![0u8; 2048];
        let mut raw_count: u64 = 0;
        let mut parsed_count: u64 = 0;

        loop {
            if cancel.is_cancelled() { break; }

            let n = unsafe {
                libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
            };
            let received_at = Instant::now();

            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut
                {
                    continue;
                }
                warn!("Raw packet capture recv error: {}", err);
                continue;
            }

            raw_count += 1;
            if let Some(payload) = extract_udp_payload(&buf[..n as usize]) {
                if let Some(key) = parse_shred_key(payload) {
                    parsed_count += 1;
                    let _ = tx.send(ShredEvent { source: source_id, key, received_at });
                }
            }
        }

        unsafe { libc::close(fd); }
        info!(
            "Raw packet capture stopped ({} packets, {} parsed as shreds)",
            raw_count, parsed_count
        );
        Ok(())
    }
}
