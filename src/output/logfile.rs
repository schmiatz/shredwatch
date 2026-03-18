use std::fs::File;
use std::io::{BufWriter, Write};
use std::time::Instant;
use serde::Serialize;
use chrono::Utc;
use anyhow::Result;

use crate::registry::{Registry, SourceId};

#[derive(Serialize)]
struct ShredLogEntry {
    slot: u64,
    index: u32,
    shred_type: String,
    first_source: String,
    first_seen_ns: u64,
    arrivals: Vec<ArrivalEntry>,
}

#[derive(Serialize)]
struct ArrivalEntry {
    source: String,
    delta_ns: u64,
}

pub fn write_log(registry: &Registry, path: &str, start: Instant) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    // Write header comment
    writeln!(writer, "// shred-bench log — {}", Utc::now().to_rfc3339())?;

    let all_sources = [
        SourceId::RawUdp,
        SourceId::JitoShredStream,
        SourceId::DoubleZero,
        SourceId::Yellowstone,
    ];

    for entry in registry.shreds.iter() {
        let key = entry.key();
        let rec = entry.value();

        let mut arrivals = Vec::new();
        for &source in &all_sources {
            if let Some(&(_, t)) = rec.arrivals.iter().find(|(s, _)| *s == source) {
                let delta = t.duration_since(rec.first_seen).as_nanos() as u64;
                arrivals.push(ArrivalEntry {
                    source: source.name().to_string(),
                    delta_ns: delta,
                });
            }
        }

        let log_entry = ShredLogEntry {
            slot: key.slot,
            index: key.index,
            shred_type: key.shred_type.to_string(),
            first_source: rec.first_source.name().to_string(),
            first_seen_ns: rec.first_seen.duration_since(start).as_nanos() as u64,
            arrivals,
        };

        let line = serde_json::to_string(&log_entry)?;
        writeln!(writer, "{}", line)?;
    }

    writer.flush()?;
    Ok(())
}
