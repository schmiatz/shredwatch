use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use clap::Parser;
use tracing::{info, warn};
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

/// Return an effective display name: use the user-configured name if set,
/// otherwise fall back to `type_default`, appending an index if there are
/// multiple enabled instances of the same type.
fn source_name(configured: &str, type_default: &str, idx: usize, total_enabled: usize) -> String {
    if !configured.is_empty() {
        configured.to_string()
    } else if total_enabled <= 1 {
        type_default.to_string()
    } else {
        format!("{} {}", type_default, idx + 1)
    }
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

    // Validate: at least one source must be defined and enabled
    let any_source = config.sources.raw_udp.iter().any(|c| c.enabled)
        || config.sources.jito.iter().any(|c| c.enabled)
        || config.sources.doublezero.iter().any(|c| c.enabled)
        || config.sources.yellowstone.iter().any(|c| c.enabled)
        || config.sources.pcap.iter().any(|c| c.enabled);
    if !any_source {
        anyhow::bail!(
            "No sources enabled in config. Define at least one source block: \
             [[sources.raw_udp]], [[sources.jito]], [[sources.doublezero]], \
             [[sources.yellowstone]], or [[sources.pcap]]."
        );
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

    // Determine run mode
    let slot_range_mode = config.start_slot.is_some() || config.end_slot.is_some();
    let start_slot_configured = config.start_slot.is_some();
    let start_slot = config.start_slot.unwrap_or(0);
    let end_slot = config.end_slot.unwrap_or(u64::MAX);
    if slot_range_mode {
        info!(
            "Slot-range mode: slots {}..{} (silence timeout: {}s)",
            start_slot,
            if end_slot == u64::MAX { "∞".to_string() } else { end_slot.to_string() },
            config.silence_timeout_secs
        );
    } else {
        let dur = config.duration_secs.max(1);
        info!("Benchmark duration: {}s (silence timeout: {}s)", dur, config.silence_timeout_secs);
    }

    let registry = Arc::new(Registry::new());
    let cancel = CancellationToken::new();
    let start_wall = Utc::now();
    let start_instant = registry.start_time;

    // Shared event channels
    let (shred_tx, mut shred_rx) = mpsc::unbounded_channel::<ShredEvent>();
    let (slot_tx, mut slot_rx) = mpsc::unbounded_channel::<SlotEvent>();

    // Sequential source ID assignment
    let mut next_id: u32 = 0;
    let mut alloc_id = || { let id = SourceId(next_id); next_id += 1; id };

    // (id, name) for each active source
    let mut active_shred_sources: Vec<(SourceId, String)> = Vec::new();
    let mut active_entry_sources: Vec<(SourceId, String)> = Vec::new();

    // name lookup for logfile
    let mut source_names: HashMap<SourceId, String> = HashMap::new();

    // ── Raw UDP sources ────────────────────────────────────────────────────────
    let total_raw_udp = config.sources.raw_udp.iter().filter(|c| c.enabled).count();
    let mut raw_udp_idx = 0;
    for cfg in config.sources.raw_udp.iter().filter(|c| c.enabled) {
        let name = source_name(&cfg.name, "Raw UDP", raw_udp_idx, total_raw_udp);
        let id = alloc_id();
        info!("Starting {} on {}", name, cfg.bind_addr);
        sources::raw_udp::run(cfg.clone(), id, shred_tx.clone(), cancel.clone()).await?;
        source_names.insert(id, name.clone());
        active_shred_sources.push((id, name));
        raw_udp_idx += 1;
    }

    // ── Jito sources ───────────────────────────────────────────────────────────
    let total_jito = config.sources.jito.iter().filter(|c| c.enabled).count();
    let mut jito_idx = 0;
    for cfg in config.sources.jito.iter().filter(|c| c.enabled) {
        let has_udp = !cfg.block_engine_url.is_empty() || !cfg.proxy_udp_addr.is_empty();
        let has_grpc = !cfg.proxy_grpc_addr.is_empty();

        let shred_name = source_name(&cfg.name, "Jito ShredStream", jito_idx, total_jito);
        let entry_name = if cfg.name.is_empty() {
            source_name("", "Jito Entries", jito_idx, total_jito)
        } else {
            format!("{} (entries)", cfg.name)
        };

        let shred_id = alloc_id();
        let entry_id = alloc_id();

        if has_udp {
            info!("Starting {} (UDP shreds)", shred_name);
            source_names.insert(shred_id, shred_name.clone());
            active_shred_sources.push((shred_id, shred_name));
        }
        if has_grpc {
            info!("Starting {} on {}", entry_name, cfg.proxy_grpc_addr);
            source_names.insert(entry_id, entry_name.clone());
            active_entry_sources.push((entry_id, entry_name));
        }

        sources::jito::run(
            cfg.clone(),
            shred_id,
            entry_id,
            shred_tx.clone(),
            slot_tx.clone(),
            cancel.clone(),
        ).await?;

        jito_idx += 1;
    }

    // ── DoubleZero sources ─────────────────────────────────────────────────────
    let total_dz = config.sources.doublezero.iter().filter(|c| c.enabled).count();
    let mut dz_idx = 0;
    for cfg in config.sources.doublezero.iter().filter(|c| c.enabled) {
        let name = source_name(&cfg.name, "DoubleZero", dz_idx, total_dz);
        let id = alloc_id();
        info!("Starting {} ({}:{})", name, cfg.multicast_group, cfg.port);
        sources::doublezero::run(cfg.clone(), id, shred_tx.clone(), cancel.clone()).await?;
        source_names.insert(id, name.clone());
        active_shred_sources.push((id, name));
        dz_idx += 1;
    }

    // ── Yellowstone sources ────────────────────────────────────────────────────
    let total_ys = config.sources.yellowstone.iter().filter(|c| c.enabled).count();
    let mut ys_idx = 0;
    for cfg in config.sources.yellowstone.iter().filter(|c| c.enabled) {
        let name = source_name(&cfg.name, "Yellowstone", ys_idx, total_ys);
        let id = alloc_id();
        info!("Starting {} on {}", name, cfg.endpoint);
        sources::yellowstone::run(cfg.clone(), id, slot_tx.clone(), cancel.clone()).await?;
        source_names.insert(id, name.clone());
        active_entry_sources.push((id, name));
        ys_idx += 1;
    }

    // ── Raw packet capture sources (AF_PACKET) ────────────────────────────────
    let total_pcap = config.sources.pcap.iter().filter(|c| c.enabled).count();
    let mut pcap_idx = 0;
    for cfg in config.sources.pcap.iter().filter(|c| c.enabled) {
        let name = source_name(&cfg.name, "Raw Capture", pcap_idx, total_pcap);
        let id = alloc_id();
        let iface = if cfg.interface.is_empty() { "all interfaces".to_string() } else { cfg.interface.clone() };
        info!("Starting {} (port={}, iface={})", name, cfg.port, iface);
        sources::pcap::run(cfg.clone(), id, shred_tx.clone(), cancel.clone()).await?;
        source_names.insert(id, name.clone());
        active_shred_sources.push((id, name));
        pcap_idx += 1;
    }

    // ── Silence-timeout monitor ────────────────────────────────────────────────
    // Fires if no shred/slot event arrives for `silence_timeout` seconds.
    let activity = Arc::new(AtomicBool::new(true)); // start true to allow warmup
    {
        let activity = Arc::clone(&activity);
        let cancel_silence = cancel.clone();
        let timeout_secs = config.silence_timeout_secs;
        tokio::spawn(async move {
            let mut silent_secs = 0u64;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if cancel_silence.is_cancelled() { break; }
                if !activity.swap(false, Ordering::Relaxed) {
                    silent_secs += 1;
                    if silent_secs >= timeout_secs {
                        warn!(
                            "No shreds received for {}s — stopping run",
                            timeout_secs
                        );
                        cancel_silence.cancel();
                        break;
                    }
                } else {
                    silent_secs = 0;
                }
            }
        });
    }

    // ── Event aggregator ───────────────────────────────────────────────────────
    let registry_agg = Arc::clone(&registry);
    let cancel_agg = cancel.clone();
    let activity_agg = Arc::clone(&activity);
    // When the first shred beyond end_slot arrives we don't cancel immediately —
    // we wait a short grace period so straggler shreds for the last slot (delayed
    // paths, Turbine retransmissions) still make it into the registry.
    let mut range_end_triggered = false;
    let mut start_slot_checked = false;

    let agg_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = shred_rx.recv() => {
                    match msg {
                        Some(event) => {
                            let slot = event.key.slot;
                            if slot < start_slot {
                                // Still waiting for range — keep timer alive so
                                // silence doesn't fire while we wait for start_slot
                                activity_agg.store(true, Ordering::Relaxed);
                                continue;
                            }
                            // First shred at or beyond start_slot: check it isn't
                            // already in the past relative to what we configured.
                            if !start_slot_checked && start_slot_configured && slot > start_slot {
                                start_slot_checked = true;
                                warn!(
                                    "start_slot {} is already in the past — \
                                     first shred received is for slot {}. \
                                     The slot range has already been missed. \
                                     Please set a future start_slot and try again.",
                                    start_slot, slot
                                );
                                cancel_agg.cancel();
                                continue;
                            }
                            start_slot_checked = true;
                            if slot > end_slot {
                                if slot_range_mode && !range_end_triggered {
                                    range_end_triggered = true;
                                    let c = cancel_agg.clone();
                                    tokio::spawn(async move {
                                        // Grace period: keep receiving in-range stragglers
                                        tokio::time::sleep(Duration::from_secs(2)).await;
                                        c.cancel();
                                    });
                                }
                                continue;
                            }
                            activity_agg.store(true, Ordering::Relaxed);
                            registry_agg.record_shred(event);
                        }
                        None => break,
                    }
                }
                msg = slot_rx.recv() => {
                    match msg {
                        Some(event) => {
                            let slot = event.slot;
                            if slot < start_slot {
                                activity_agg.store(true, Ordering::Relaxed);
                                continue;
                            }
                            if slot > end_slot {
                                if slot_range_mode && !range_end_triggered {
                                    range_end_triggered = true;
                                    let c = cancel_agg.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_secs(2)).await;
                                        c.cancel();
                                    });
                                }
                                continue;
                            }
                            activity_agg.store(true, Ordering::Relaxed);
                            registry_agg.record_slot_event(event);
                        }
                        None => break,
                    }
                }
            }
        }
    });

    // ── Main wait ──────────────────────────────────────────────────────────────
    info!("Benchmark running... press Ctrl+C to stop early");

    if config.duration_secs > 0 && !slot_range_mode {
        let duration = Duration::from_secs(config.duration_secs);
        tokio::select! {
            _ = tokio::time::sleep(duration) => { info!("Duration elapsed, stopping..."); }
            _ = cancel.cancelled() => { info!("Stopped early"); }
            _ = tokio::signal::ctrl_c() => { info!("Ctrl+C received, stopping..."); }
        }
    } else {
        tokio::select! {
            _ = cancel.cancelled() => { info!("Run complete (slot range or silence timeout)"); }
            _ = tokio::signal::ctrl_c() => { info!("Ctrl+C received, stopping..."); }
        }
    }

    // Stop all sources
    cancel.cancel();

    // Give listeners a moment to drain
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(shred_tx);
    drop(slot_tx);

    let _ = tokio::time::timeout(Duration::from_secs(2), agg_task).await;

    let actual_duration = Instant::now().duration_since(start_instant).as_secs_f64();
    info!(
        "Collected {} unique shreds across {} slots",
        registry.shreds.len(),
        registry.slots.len()
    );

    let bench_stats = compute_stats(&registry, &active_shred_sources, &active_entry_sources, actual_duration);
    print_results(&bench_stats, start_wall);

    if let Some(path) = log_path {
        info!("Writing log to {}", path);
        write_log(&registry, &path, start_instant, &source_names)
            .with_context(|| format!("Failed to write log: {}", path))?;
        println!("  Log written to: {}", path);
    }

    Ok(())
}
