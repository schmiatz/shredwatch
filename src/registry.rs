use std::time::Instant;
use dashmap::DashMap;
use crate::shred::ShredKey;

/// ID for each shred source
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceId {
    RawUdp = 0,
    JitoShredStream = 1, // raw UDP shreds from jito-shredstream-proxy --dest-ip-ports
    DoubleZero = 2,
    Yellowstone = 3,     // slot-level: SLOT_FIRST_SHRED_RECEIVED
    JitoEntries = 4,     // entry-level: proxy --grpc-service-port SubscribeEntries
}

impl Default for SourceId {
    fn default() -> Self {
        SourceId::RawUdp
    }
}

impl SourceId {
    pub fn name(&self) -> &'static str {
        match self {
            SourceId::RawUdp => "Raw UDP",
            SourceId::JitoShredStream => "Jito ShredStream (UDP)",
            SourceId::DoubleZero => "DoubleZero",
            SourceId::Yellowstone => "Yellowstone gRPC",
            SourceId::JitoEntries => "Jito Entries (gRPC)",
        }
    }
}

/// Event from a shred-level source (Raw UDP, Jito UDP, DoubleZero)
pub struct ShredEvent {
    pub source: SourceId,
    pub key: ShredKey,
    pub received_at: Instant,
}

/// Event from an entry/slot-level source (Yellowstone, Jito gRPC entries)
pub struct SlotEvent {
    pub source: SourceId,
    pub slot: u64,
    pub received_at: Instant,
}

/// Per-shred record in the registry
pub struct ShredRecord {
    pub first_seen: Instant,
    pub first_source: SourceId,
    /// All arrivals: (source, time). May have multiple from same source (dupes).
    pub arrivals: Vec<(SourceId, Instant)>,
}

/// Per-slot record
pub struct SlotRecord {
    /// Earliest arrival across all shred-level sources
    pub first_shred_at: Instant,
    /// First arrival per entry-level source (Yellowstone, JitoEntries, etc.)
    /// Stores (SourceId, earliest_arrival_time).
    pub entry_arrivals: Vec<(SourceId, Instant)>,
}

pub struct Registry {
    pub shreds: DashMap<ShredKey, ShredRecord>,
    pub slots: DashMap<u64, SlotRecord>,
    pub start_time: Instant,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            shreds: DashMap::new(),
            slots: DashMap::new(),
            start_time: Instant::now(),
        }
    }

    pub fn record_shred(&self, event: ShredEvent) {
        let slot = event.key.slot;

        self.shreds
            .entry(event.key)
            .and_modify(|rec| {
                rec.arrivals.push((event.source, event.received_at));
            })
            .or_insert_with(|| ShredRecord {
                first_seen: event.received_at,
                first_source: event.source,
                arrivals: vec![(event.source, event.received_at)],
            });

        // Track earliest shred arrival per slot
        self.slots
            .entry(slot)
            .and_modify(|rec| {
                if event.received_at < rec.first_shred_at {
                    rec.first_shred_at = event.received_at;
                }
            })
            .or_insert_with(|| SlotRecord {
                first_shred_at: event.received_at,
                entry_arrivals: vec![],
            });
    }

    pub fn record_slot_event(&self, event: SlotEvent) {
        self.slots
            .entry(event.slot)
            .and_modify(|rec| {
                // Keep the minimum arrival time per source
                if let Some(existing) = rec
                    .entry_arrivals
                    .iter_mut()
                    .find(|(s, _)| *s == event.source)
                {
                    if event.received_at < existing.1 {
                        existing.1 = event.received_at;
                    }
                } else {
                    rec.entry_arrivals.push((event.source, event.received_at));
                }
            })
            .or_insert_with(|| SlotRecord {
                // If no shred event seen yet for this slot, use this as approximation
                first_shred_at: event.received_at,
                entry_arrivals: vec![(event.source, event.received_at)],
            });
    }
}
