use std::net::UdpSocket;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use anyhow::Result;

use crate::config::RawUdpConfig;
use crate::registry::{ShredEvent, SourceId};
use crate::shred::parse_shred_key;

pub async fn run(
    config: RawUdpConfig,
    tx: mpsc::UnboundedSender<ShredEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    let bind_addr = config.bind_addr.clone();

    tokio::task::spawn_blocking(move || {
        let socket = match UdpSocket::bind(&bind_addr) {
            Ok(s) => s,
            Err(e) => {
                error!("Raw UDP: failed to bind {}: {}", bind_addr, e);
                return;
            }
        };

        // Set receive buffer size using socket2 via try_clone
        if config.recv_buf_size > 0 {
            if let Ok(cloned) = socket.try_clone() {
                // Convert std socket to socket2 for buffer size setting
                let s2 = socket2::Socket::from(cloned);
                let _ = s2.set_recv_buffer_size(config.recv_buf_size);
                // s2 is dropped here which closes its fd (the clone), but the original socket remains open
            }
        }

        // Set 100ms read timeout so we can check cancellation
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .ok();

        info!("Raw UDP listener started on {}", bind_addr);
        let mut buf = vec![0u8; 1280];

        loop {
            if cancel.is_cancelled() {
                break;
            }

            match socket.recv_from(&mut buf) {
                Ok((len, _addr)) => {
                    let received_at = Instant::now();
                    if let Some(key) = parse_shred_key(&buf[..len]) {
                        let _ = tx.send(ShredEvent {
                            source: SourceId::RawUdp,
                            key,
                            received_at,
                        });
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // timeout — check cancellation on next iteration
                }
                Err(e) => {
                    warn!("Raw UDP recv error: {}", e);
                }
            }
        }
        info!("Raw UDP listener stopped");
    });

    Ok(())
}
