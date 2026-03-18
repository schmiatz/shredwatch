use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use clap::Parser;
use tracing::info;
use chrono::Utc;
use anyhow::{Context, Result};

mod config;
mod shred;
mod registry;
mod stats;
mod sources;
mod output;

use config::Config;
use registry::{Registry, ShredEvent, SlotEvent, SourceId};
use stats::compute_stats;
use output::table::print_results;
use output::logfile::write_log;

#[derive(Parser, Debug)]
#[command(name = "shredwatch", about = "Solana shred source latency benchmark")]
struct Args {
    /// Path to TOML config file
    #[arg(short, long, default_value = "config.toml")]
    config: String,

    /// Benchmark duration in seconds (overrides config)
    #[arg(short, long)]
    duration: Option<u64>,

    /// Log file path (overrides config)
    #[arg(short, long)]
    log_file: Option<String>,

    /// Disable log file output
    #[arg(long)]
    no_log: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("shredwatch=info".parse()?),
        )
        .init();

    let args = Args::parse();

    let cfg_text = std::fs::read_to_string(&args.config)
        .with_context(|| format!("Failed to read config: {}", args.config))?;
    let mut config: Config = toml::from_str(&cfg_text)
        .with_context(|| "Failed to parse config TOML")?;

    if let Some(d) = args.duration {
        config.duration_secs = d;
    }

    let log_path = if args.no_log {
        None
    } else {
        args.log_file.clone().or_else(|| {
            if config.output.log_file.is_empty() {
                None
            } else {
                Some(config.output.log_file.clone())
            }
        })
    };

    let duration = Duration::from_secs(config.duration_secs.max(1));
    info!("Benchmark duration: {}s", duration.as_secs());

    let registry = Arc::new(Registry::new());
    let cancel = CancellationToken::new();
    let start_wall = Utc::now();
    let start_instant = registry.start_time;

    // Shared event channels
    let (shred_tx, mut shred_rx) = mpsc::unbounded_channel::<ShredEvent>();
    let (slot_tx, mut slot_rx) = mpsc::unbounded_channel::<SlotEvent>();

    // Shred-level sources (compared per (slot, index, shred_type))
    let mut active_shred_sources: Vec<SourceId> = Vec::new();
    // Entry/slot-level sources (compared per slot, shown in separate table)
    let mut active_entry_sources: Vec<SourceId> = Vec::new();

    // Start sources
    if config.sources.raw_udp.enabled {
        info!(
            "Starting Raw UDP source on {}",
            config.sources.raw_udp.bind_addr
        );
        sources::raw_udp::run(
            config.sources.raw_udp.clone(),
            shred_tx.clone(),
            cancel.clone(),
        )
        .await?;
        active_shred_sources.push(SourceId::RawUdp);
    }

    if config.sources.jito.enabled {
        let cfg = &config.sources.jito;
        // Direct mode and proxy UDP both produce shred-level data
        let has_udp = !cfg.block_engine_url.is_empty() || !cfg.proxy_udp_addr.is_empty();
        if has_udp {
            info!("Starting Jito ShredStream (UDP shreds)");
            active_shred_sources.push(SourceId::JitoShredStream);
        }
        if !cfg.proxy_grpc_addr.is_empty() {
            info!("Starting Jito gRPC entries on {}", cfg.proxy_grpc_addr);
            active_entry_sources.push(SourceId::JitoEntries);
        }
        sources::jito::run(
            config.sources.jito.clone(),
            shred_tx.clone(),
            slot_tx.clone(),
            cancel.clone(),
        )
        .await?;
    }

    if config.sources.doublezero.enabled {
        info!("Starting DoubleZero source");
        sources::doublezero::run(
            config.sources.doublezero.clone(),
            shred_tx.clone(),
            cancel.clone(),
        )
        .await?;
        active_shred_sources.push(SourceId::DoubleZero);
    }

    if config.sources.yellowstone.enabled {
        info!("Starting Yellowstone gRPC source");
        sources::yellowstone::run(
            config.sources.yellowstone.clone(),
            slot_tx.clone(),
            cancel.clone(),
        )
        .await?;
        active_entry_sources.push(SourceId::Yellowstone);
    }

    // Event aggregator task
    let registry_clone = Arc::clone(&registry);
    let agg_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = shred_rx.recv() => {
                    match msg {
                        Some(event) => registry_clone.record_shred(event),
                        None => break,
                    }
                }
                msg = slot_rx.recv() => {
                    match msg {
                        Some(event) => registry_clone.record_slot_event(event),
                        None => break,
                    }
                }
            }
        }
    });

    // Wait for duration or Ctrl+C
    info!("Benchmark running... press Ctrl+C to stop early");
    tokio::select! {
        _ = tokio::time::sleep(duration) => {
            info!("Duration elapsed, stopping...");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl+C received, stopping...");
        }
    }

    // Stop all sources
    cancel.cancel();

    // Give listeners a moment to drain
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(shred_tx);
    drop(slot_tx);

    // Wait for aggregator
    let _ = tokio::time::timeout(Duration::from_secs(2), agg_task).await;

    let actual_duration = Instant::now().duration_since(start_instant).as_secs_f64();
    info!(
        "Collected {} unique shreds across {} slots",
        registry.shreds.len(),
        registry.slots.len()
    );

    // Compute and print stats
    let bench_stats = compute_stats(&registry, &active_shred_sources, &active_entry_sources, actual_duration);
    print_results(&bench_stats, start_wall);

    // Write log file
    if let Some(path) = log_path {
        info!("Writing log to {}", path);
        write_log(&registry, &path, start_instant)
            .with_context(|| format!("Failed to write log: {}", path))?;
        println!("  Log written to: {}", path);
    }

    Ok(())
}
