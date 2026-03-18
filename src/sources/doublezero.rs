use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::str::FromStr;
use std::time::{Duration, Instant};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use anyhow::Result;

use crate::config::DoubleZeroConfig;
use crate::registry::{ShredEvent, SourceId};
use crate::shred::parse_shred_key;

pub async fn run(
    config: DoubleZeroConfig,
    tx: mpsc::UnboundedSender<ShredEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let multicast_ip = match Ipv4Addr::from_str(&config.multicast_group) {
            Ok(ip) => ip,
            Err(e) => {
                error!("Invalid multicast group IP: {}", e);
                return;
            }
        };

        // Create a UDP socket with SO_REUSEADDR
        let socket = match Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)) {
            Ok(s) => s,
            Err(e) => {
                error!("DoubleZero: socket create failed: {}", e);
                return;
            }
        };

        socket.set_reuse_address(true).ok();
        #[cfg(unix)]
        socket.set_reuse_port(true).ok();

        let bind_addr: SocketAddr = format!("0.0.0.0:{}", config.port).parse().unwrap();
        if let Err(e) = socket.bind(&bind_addr.into()) {
            error!("DoubleZero: bind failed: {}", e);
            return;
        }

        // Look up interface by name to get its IP
        let iface_ip = get_interface_ip(&config.interface).unwrap_or(Ipv4Addr::UNSPECIFIED);

        // Join multicast group
        if let Err(e) = socket.join_multicast_v4(&multicast_ip, &iface_ip) {
            error!("DoubleZero: join_multicast_v4 failed: {}", e);
            return;
        }

        // Disable receiving multicast from all groups system-wide (IP_MULTICAST_ALL=0)
        // This ensures we only get packets for groups we explicitly joined
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::io::AsRawFd;
            let fd = socket.as_raw_fd();
            let val: libc::c_int = 0;
            libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_MULTICAST_ALL,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }

        socket.set_read_timeout(Some(Duration::from_millis(100))).ok();

        let std_socket: UdpSocket = socket.into();
        info!(
            "DoubleZero multicast listener started (group={}, port={}, iface={})",
            multicast_ip, config.port, config.interface
        );

        let mut buf = vec![0u8; 1280];
        loop {
            if cancel.is_cancelled() {
                break;
            }
            match std_socket.recv_from(&mut buf) {
                Ok((len, _)) => {
                    let received_at = Instant::now();
                    if let Some(key) = parse_shred_key(&buf[..len]) {
                        let _ = tx.send(ShredEvent {
                            source: SourceId::DoubleZero,
                            key,
                            received_at,
                        });
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    warn!("DoubleZero recv error: {}", e);
                }
            }
        }
        info!("DoubleZero listener stopped");
    });

    Ok(())
}

fn get_interface_ip(iface_name: &str) -> Option<Ipv4Addr> {
    // Try to resolve interface name to IP via `ip addr show <iface>`
    use std::process::Command;
    let output = Command::new("ip")
        .args(["addr", "show", iface_name])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&output.stdout);
    for line in s.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("inet ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if let Some(addr_cidr) = parts.get(1) {
                let addr = addr_cidr.split('/').next()?;
                return Ipv4Addr::from_str(addr).ok();
            }
        }
    }
    None
}
