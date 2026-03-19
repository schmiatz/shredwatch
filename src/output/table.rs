use tabled::{settings::Style, Table, Tabled};
use crate::stats::{compute_percentiles, BenchmarkStats};

fn fmt_ns(ns: u64) -> String {
    if ns == 0 {
        return "  0 µs".to_string();
    }
    if ns < 1_000 {
        format!("{:>3} ns", ns)
    } else if ns < 1_000_000 {
        format!("{:>5.1} µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:>5.1} ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:>5.2}  s", ns as f64 / 1_000_000_000.0)
    }
}

fn coverage(received: u64, total: u64) -> String {
    if total == 0 {
        return "  N/A".to_string();
    }
    format!("{:>5.1}%", received as f64 / total as f64 * 100.0)
}

fn num_fmt(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

#[derive(Tabled)]
struct LatencyRow {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Shreds")]
    shreds: String,
    #[tabled(rename = "  p50  ")]
    p50: String,
    #[tabled(rename = "  p90  ")]
    p90: String,
    #[tabled(rename = "  p95  ")]
    p95: String,
    #[tabled(rename = "  p99  ")]
    p99: String,
    #[tabled(rename = " p99.9 ")]
    p99_9: String,
    #[tabled(rename = "   max  ")]
    max: String,
}

#[derive(Tabled)]
struct WinRow {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Won First")]
    wins: String,
    #[tabled(rename = "Win Rate")]
    win_rate: String,
}

#[derive(Tabled)]
struct CoverageRow {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Received")]
    received: String,
    #[tabled(rename = "Coverage")]
    coverage: String,
    #[tabled(rename = "Dupes")]
    dupes: String,
    #[tabled(rename = "Missed")]
    missed: String,
}

#[derive(Tabled)]
struct ShredTypeRow {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Data Shreds")]
    data: String,
    #[tabled(rename = "FEC Shreds")]
    code: String,
}

#[derive(Tabled)]
struct SlotRow {
    #[tabled(rename = "Source")]
    source: String,
    #[tabled(rename = "Slots")]
    slots: String,
    #[tabled(rename = "  p50  ")]
    p50: String,
    #[tabled(rename = "  p90  ")]
    p90: String,
    #[tabled(rename = "  p95  ")]
    p95: String,
    #[tabled(rename = "  p99  ")]
    p99: String,
    #[tabled(rename = "   max  ")]
    max: String,
}

pub fn print_results(stats: &BenchmarkStats, start_time: chrono::DateTime<chrono::Utc>) {
    let total = stats.total_unique_shreds;

    // Header
    let title = format!(
        " SHRED BENCHMARK  ·  {:.0}s  ·  {} unique shreds  ·  {} sources ",
        stats.duration_secs,
        num_fmt(total),
        stats.sources.len()
    );
    let width = 90;
    println!();
    println!("╔{}╗", "═".repeat(width));
    println!("║{:^width$}║", title, width = width);
    println!(
        "║{:^width$}║",
        format!(
            "started {}",
            start_time.format("%Y-%m-%d %H:%M:%S UTC")
        ),
        width = width
    );
    println!("╚{}╝", "═".repeat(width));
    println!();

    // Latency table
    println!("LATENCY RELATIVE TO FIRST ARRIVAL  (per shred, data + FEC combined)");
    let latency_rows: Vec<LatencyRow> = stats
        .sources
        .iter()
        .map(|s| {
            let p = compute_percentiles(s.latency_ns.clone());
            LatencyRow {
                source: s.name.clone(),
                shreds: num_fmt(s.received),
                p50: fmt_ns(p.p50),
                p90: fmt_ns(p.p90),
                p95: fmt_ns(p.p95),
                p99: fmt_ns(p.p99),
                p99_9: fmt_ns(p.p99_9),
                max: fmt_ns(p.max),
            }
        })
        .collect();
    println!("{}", Table::new(latency_rows).with(Style::sharp()));
    println!();

    // First-arrival wins
    println!("FIRST ARRIVAL WINS  (which source received each shred first)");
    let win_rows: Vec<WinRow> = stats
        .sources
        .iter()
        .map(|s| {
            let rate = if total > 0 {
                format!("{:.1}%", s.wins as f64 / total as f64 * 100.0)
            } else {
                "N/A".to_string()
            };
            WinRow {
                source: s.name.clone(),
                wins: num_fmt(s.wins),
                win_rate: rate,
            }
        })
        .collect();
    println!("{}", Table::new(win_rows).with(Style::sharp()));
    println!();

    // Coverage
    println!("COVERAGE & RELIABILITY");
    let cov_rows: Vec<CoverageRow> = stats
        .sources
        .iter()
        .map(|s| CoverageRow {
            source: s.name.clone(),
            received: num_fmt(s.received),
            coverage: coverage(s.received, total),
            dupes: num_fmt(s.dupes),
            missed: num_fmt(s.missed),
        })
        .collect();
    println!("{}", Table::new(cov_rows).with(Style::sharp()));
    println!();

    // Shred type breakdown
    println!("SHRED TYPE BREAKDOWN");
    let type_rows: Vec<ShredTypeRow> = stats
        .sources
        .iter()
        .map(|s| {
            let total_s = s.data_shreds + s.code_shreds;
            let data_pct = if total_s > 0 {
                s.data_shreds * 100 / total_s
            } else {
                0
            };
            let code_pct = if total_s > 0 {
                s.code_shreds * 100 / total_s
            } else {
                0
            };
            ShredTypeRow {
                source: s.name.clone(),
                data: format!("{} ({}%)", num_fmt(s.data_shreds), data_pct),
                code: format!("{} ({}%)", num_fmt(s.code_shreds), code_pct),
            }
        })
        .collect();
    println!("{}", Table::new(type_rows).with(Style::sharp()));
    println!();

    // Entry/slot-level sources (Yellowstone, Jito gRPC entries, etc.)
    let entry_rows: Vec<SlotRow> = stats
        .slot_stats
        .entry_sources
        .iter()
        .filter(|s| s.slots_seen > 0)
        .map(|s| {
            let p = compute_percentiles(s.latency_ns.clone());
            SlotRow {
                source: s.name.clone(),
                slots: num_fmt(s.slots_seen),
                p50: fmt_ns(p.p50),
                p90: fmt_ns(p.p90),
                p95: fmt_ns(p.p95),
                p99: fmt_ns(p.p99),
                max: fmt_ns(p.max),
            }
        })
        .collect();
    if !entry_rows.is_empty() {
        println!("SLOT / ENTRY LATENCY vs first raw shred arrival");
        println!("(these sources deliver assembled entries, not raw shreds — slot granularity)");
        println!("{}", Table::new(entry_rows).with(Style::sharp()));
        println!();
    }

    println!(
        "  Total unique shreds: {}  ·  Total slots observed: {}",
        num_fmt(total),
        num_fmt(stats.slot_stats.total_slots)
    );
    println!();
}
