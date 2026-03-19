use std::time::Instant;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{info, warn};
use anyhow::Result;
use futures::StreamExt;

use crate::config::YellowstoneConfig;
use crate::registry::{SlotEvent, SourceId};

pub mod proto {
    pub mod geyser {
        tonic::include_proto!("geyser");
    }
}

use proto::geyser::geyser_client::GeyserClient;
use proto::geyser::{
    subscribe_update::UpdateOneof, SlotStatus, SubscribeRequest, SubscribeRequestFilterSlots,
};

pub async fn run(
    config: YellowstoneConfig,
    source_id: SourceId,
    tx: mpsc::UnboundedSender<SlotEvent>,
    cancel: CancellationToken,
) -> Result<()> {
    tokio::spawn(async move {
        loop {
            if cancel.is_cancelled() {
                break;
            }

            match connect_and_stream(&config, &tx, &cancel, source_id).await {
                Ok(()) => break,
                Err(e) => {
                    warn!("Yellowstone stream error: {}, reconnecting...", e);
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                        _ = cancel.cancelled() => { break; }
                    }
                }
            }
        }
        info!("Yellowstone listener stopped");
    });

    Ok(())
}

async fn connect_and_stream(
    config: &YellowstoneConfig,
    tx: &mpsc::UnboundedSender<SlotEvent>,
    cancel: &CancellationToken,
    source_id: SourceId,
) -> anyhow::Result<()> {
    let channel = Channel::from_shared(config.endpoint.clone())?
        .connect()
        .await?;

    info!("Yellowstone: connected to {}", config.endpoint);

    // Subscribe to slot updates only
    let mut slots_filter = HashMap::new();
    slots_filter.insert(
        "bench".to_string(),
        SubscribeRequestFilterSlots {
            filter_by_commitment: None,
            interslot_updates: Some(true),
        },
    );

    let request = SubscribeRequest {
        slots: slots_filter,
        accounts: HashMap::new(),
        transactions: HashMap::new(),
        transactions_status: HashMap::new(),
        blocks: HashMap::new(),
        blocks_meta: HashMap::new(),
        entry: HashMap::new(),
        commitment: None,
        accounts_data_slice: vec![],
        ping: None,
    };

    let (tx_req, rx_req) = tokio::sync::mpsc::channel(4);
    tx_req.send(request).await?;

    // Build a tonic request stream, adding auth token if configured
    let x_token = config.x_token.clone();
    let mut grpc_request =
        tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(rx_req));
    if !x_token.is_empty() {
        grpc_request.metadata_mut().insert(
            "x-token",
            x_token.parse().map_err(|e: tonic::metadata::errors::InvalidMetadataValue| {
                anyhow::anyhow!("Invalid x-token metadata value: {}", e)
            })?,
        );
    }

    let mut client = GeyserClient::new(channel);
    let response = client.subscribe(grpc_request).await?;
    let mut stream = response.into_inner();

    loop {
        let msg: Option<Result<proto::geyser::SubscribeUpdate, tonic::Status>> =
            tokio::select! {
                msg = stream.next() => msg,
                _ = cancel.cancelled() => None,
            };

        match msg {
            Some(Ok(update)) => {
                if let Some(UpdateOneof::Slot(slot_update)) = update.update_oneof {
                    if slot_update.status == SlotStatus::SlotFirstShredReceived as i32 {
                        let received_at = Instant::now();
                        let _ = tx.send(SlotEvent {
                            source: source_id,
                            slot: slot_update.slot,
                            received_at,
                        });
                    }
                }
            }
            Some(Err(e)) => return Err(e.into()),
            None => break,
        }
    }

    Ok(())
}
