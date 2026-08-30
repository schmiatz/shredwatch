use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use reed_solomon_erasure::galois_8::ReedSolomon;

use crate::registry::{ReconstructionEvent, ShredEvent, SourceId};
use crate::shred::ShredType;

const SLOT_RETENTION: u64 = 128;
const MAX_DATA_SHREDS_PER_SLOT: u32 = 32_768;
// Agave 3.1 Merkle shred wire constants. Keeping these local avoids importing
// blockstore/runtime through the monolithic solana-ledger crate.
const DATA_PAYLOAD_SIZE: usize = 1_203;
const CODE_PAYLOAD_SIZE: usize = 1_228;
const SIGNATURE_SIZE: usize = 64;
const DATA_HEADER_SIZE: usize = 88;
const CODE_HEADER_SIZE: usize = 89;
const MERKLE_ROOT_SIZE: usize = 32;
const MERKLE_PROOF_ENTRY_SIZE: usize = 20;
const HASH_SIZE: usize = 32;
const TRANSACTION_SIGNATURE_SIZE: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WireKind {
    Data {
        data_complete: bool,
        last_in_slot: bool,
    },
    Code {
        num_data: usize,
        num_code: usize,
        position: usize,
    },
}

#[derive(Clone)]
struct WireShred {
    bytes: Arc<[u8]>,
    shard_range: Range<usize>,
    data_range: Option<Range<usize>>,
    slot: u64,
    index: u32,
    fec_set_index: u32,
    proof_size: u8,
    resigned: bool,
    signature: [u8; SIGNATURE_SIZE],
    kind: WireKind,
}

impl WireShred {
    fn parse(event: &ShredEvent) -> Option<Self> {
        let bytes = Arc::clone(&event.payload);
        let variant = *bytes.get(64)?;
        let (is_data, proof_size, resigned, payload_size, header_size) = match variant & 0xf0 {
            0x90 => (
                true,
                variant & 0x0f,
                false,
                DATA_PAYLOAD_SIZE,
                DATA_HEADER_SIZE,
            ),
            0xb0 => (
                true,
                variant & 0x0f,
                true,
                DATA_PAYLOAD_SIZE,
                DATA_HEADER_SIZE,
            ),
            0x60 => (
                false,
                variant & 0x0f,
                false,
                CODE_PAYLOAD_SIZE,
                CODE_HEADER_SIZE,
            ),
            0x70 => (
                false,
                variant & 0x0f,
                true,
                CODE_PAYLOAD_SIZE,
                CODE_HEADER_SIZE,
            ),
            _ => return None,
        };
        if bytes.len() < payload_size {
            return None;
        }

        let slot = read_u64(&bytes, 65)?;
        let index = read_u32(&bytes, 73)?;
        let fec_set_index = read_u32(&bytes, 79)?;
        if event.key.slot != slot
            || event.key.index != index
            || matches!(event.key.shred_type, ShredType::Data) != is_data
        {
            return None;
        }

        let trailer_size = MERKLE_ROOT_SIZE
            .checked_add(usize::from(proof_size).checked_mul(MERKLE_PROOF_ENTRY_SIZE)?)?
            .checked_add(if resigned { SIGNATURE_SIZE } else { 0 })?;
        let shard_end = payload_size.checked_sub(trailer_size)?;
        if shard_end < header_size {
            return None;
        }

        let signature = bytes.get(..SIGNATURE_SIZE)?.try_into().ok()?;
        let (shard_start, data_range, kind) = if is_data {
            if index < fec_set_index || index >= MAX_DATA_SHREDS_PER_SLOT {
                return None;
            }
            let flags = *bytes.get(85)?;
            let size = usize::from(read_u16(&bytes, 86)?);
            if !(DATA_HEADER_SIZE..=shard_end).contains(&size) {
                return None;
            }
            (
                SIGNATURE_SIZE,
                Some(DATA_HEADER_SIZE..size),
                WireKind::Data {
                    data_complete: flags & 0x40 != 0,
                    last_in_slot: flags & 0xc0 == 0xc0,
                },
            )
        } else {
            let num_data = usize::from(read_u16(&bytes, 83)?);
            let num_code = usize::from(read_u16(&bytes, 85)?);
            let position = usize::from(read_u16(&bytes, 87)?);
            let total = num_data.checked_add(num_code)?;
            if num_data == 0 || num_code == 0 || total > 256 || position >= num_code {
                return None;
            }
            let data_end = fec_set_index.checked_add(u32::try_from(num_data).ok()?)?;
            if data_end > MAX_DATA_SHREDS_PER_SLOT {
                return None;
            }
            (
                CODE_HEADER_SIZE,
                None,
                WireKind::Code {
                    num_data,
                    num_code,
                    position,
                },
            )
        };

        Some(Self {
            bytes,
            shard_range: shard_start..shard_end,
            data_range,
            slot,
            index,
            fec_set_index,
            proof_size,
            resigned,
            signature,
            kind,
        })
    }

    fn from_recovered_data(
        shard: Vec<u8>,
        slot: u64,
        index: u32,
        fec_set_index: u32,
        proof_size: u8,
        resigned: bool,
        signature: [u8; SIGNATURE_SIZE],
    ) -> Option<Self> {
        // A data erasure shard begins at the variant byte, so all common and
        // data header offsets are shifted left by the 64-byte signature.
        let variant = *shard.first()?;
        let expected_variant = (if resigned { 0xb0 } else { 0x90 }) | proof_size;
        let recovered_slot = read_u64(&shard, 1)?;
        let recovered_index = read_u32(&shard, 9)?;
        let recovered_fec_set = read_u32(&shard, 15)?;
        let flags = *shard.get(21)?;
        let size = usize::from(read_u16(&shard, 22)?);
        let data_end = size.checked_sub(SIGNATURE_SIZE)?;
        if variant != expected_variant
            || recovered_slot != slot
            || recovered_index != index
            || recovered_fec_set != fec_set_index
            || !(24..=shard.len()).contains(&data_end)
        {
            return None;
        }
        let shard_len = shard.len();
        Some(Self {
            bytes: Arc::from(shard),
            shard_range: 0..shard_len,
            data_range: Some(24..data_end),
            slot,
            index,
            fec_set_index,
            proof_size,
            resigned,
            signature,
            kind: WireKind::Data {
                data_complete: flags & 0x40 != 0,
                last_in_slot: flags & 0xc0 == 0xc0,
            },
        })
    }

    fn is_data(&self) -> bool {
        matches!(self.kind, WireKind::Data { .. })
    }

    fn is_code(&self) -> bool {
        matches!(self.kind, WireKind::Code { .. })
    }

    fn data_complete(&self) -> bool {
        matches!(
            self.kind,
            WireKind::Data {
                data_complete: true,
                ..
            }
        )
    }

    fn last_in_slot(&self) -> bool {
        matches!(
            self.kind,
            WireKind::Data {
                last_in_slot: true,
                ..
            }
        )
    }

    fn erasure_shard(&self) -> &[u8] {
        &self.bytes[self.shard_range.clone()]
    }

    fn data(&self) -> Option<&[u8]> {
        Some(&self.bytes[self.data_range.clone()?])
    }
}

#[derive(Default)]
struct FecAssembly {
    shreds: HashMap<(bool, u32), WireShred>,
    erasure_config: Option<(usize, usize)>,
    last_recovery_attempt_size: usize,
    recovered_data: bool,
}

struct SlotAssembly {
    first_received_at: Instant,
    last_received_at: Instant,
    data_indexes: HashSet<u32>,
    fec_sets: HashMap<u32, FecAssembly>,
    last_data_index: Option<u32>,
    received_shreds: u32,
    recovered_data_shreds: u32,
    fec_sets_recovered: u32,
    reconstruction_cpu_ns: u64,
}

impl SlotAssembly {
    fn new(received_at: Instant) -> Self {
        Self {
            first_received_at: received_at,
            last_received_at: received_at,
            data_indexes: HashSet::new(),
            fec_sets: HashMap::new(),
            last_data_index: None,
            received_shreds: 0,
            recovered_data_shreds: 0,
            fec_sets_recovered: 0,
            reconstruction_cpu_ns: 0,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct DecodedBlock {
    entries: u64,
    transactions: u64,
}

#[derive(Default)]
pub struct ReconstructionTracker {
    slots: HashMap<(SourceId, u64), SlotAssembly>,
    completed: HashSet<(SourceId, u64)>,
    newest_slots: HashMap<SourceId, u64>,
}

impl ReconstructionTracker {
    pub fn record(&mut self, event: &ShredEvent) -> Option<ReconstructionEvent> {
        let shred = WireShred::parse(event)?;
        let slot = shred.slot;
        let newest_slot = self.newest_slots.entry(event.source).or_default();
        *newest_slot = (*newest_slot).max(slot);
        let newest_slot = *newest_slot;
        self.prune(event.source, newest_slot);
        if slot < newest_slot.saturating_sub(SLOT_RETENTION) {
            return None;
        }

        let slot_key = (event.source, slot);
        if self.completed.contains(&slot_key) {
            return None;
        }

        let fec_set_index = shred.fec_set_index;
        let shred_key = (shred.is_code(), shred.index);
        if let Some(fec) = self
            .slots
            .get(&slot_key)
            .and_then(|state| state.fec_sets.get(&fec_set_index))
        {
            if fec.shreds.contains_key(&shred_key)
                || fec.shreds.values().any(|existing| {
                    existing.proof_size != shred.proof_size
                        || existing.resigned != shred.resigned
                        || existing.signature != shred.signature
                })
            {
                return None;
            }
            match shred.kind {
                WireKind::Data { .. } => {
                    if let Some((num_data, _)) = fec.erasure_config {
                        let Ok(position) = usize::try_from(shred.index - fec_set_index) else {
                            return None;
                        };
                        if position >= num_data {
                            return None;
                        }
                    }
                }
                WireKind::Code {
                    num_data, num_code, ..
                } => {
                    let config = (num_data, num_code);
                    if fec
                        .erasure_config
                        .is_some_and(|existing| existing != config)
                        || fec
                            .shreds
                            .values()
                            .filter(|existing| existing.is_data())
                            .any(|existing| {
                                usize::try_from(existing.index - fec_set_index)
                                    .map_or(true, |position| position >= num_data)
                            })
                    {
                        return None;
                    }
                }
            }
        }

        let state = self
            .slots
            .entry(slot_key)
            .or_insert_with(|| SlotAssembly::new(event.received_at));
        let fec = state.fec_sets.entry(fec_set_index).or_default();
        if let WireKind::Code {
            num_data, num_code, ..
        } = shred.kind
        {
            fec.erasure_config = Some((num_data, num_code));
        }
        state.received_shreds = state.received_shreds.saturating_add(1);
        state.first_received_at = state.first_received_at.min(event.received_at);
        state.last_received_at = state.last_received_at.max(event.received_at);
        if shred.is_data() {
            state.data_indexes.insert(shred.index);
            if shred.last_in_slot() {
                state.last_data_index = Some(shred.index);
            }
        }
        fec.shreds.insert(shred_key, shred);

        Self::try_recovery(state, fec_set_index);

        let last_data_index = state.last_data_index?;
        if last_data_index >= MAX_DATA_SHREDS_PER_SLOT
            || state.data_indexes.len() < last_data_index as usize + 1
            || !(0..=last_data_index).all(|index| state.data_indexes.contains(&index))
        {
            return None;
        }

        let started = Instant::now();
        let decoded = validate_and_decode_slot(state, last_data_index);
        state.reconstruction_cpu_ns = state
            .reconstruction_cpu_ns
            .saturating_add(elapsed_ns(started));
        let decoded = decoded?;

        let state = self.slots.remove(&slot_key)?;
        self.completed.insert(slot_key);
        Some(ReconstructionEvent {
            source: event.source,
            slot,
            reconstructable_at: state.last_received_at,
            first_source_shred_at: state.first_received_at,
            expected_data_shreds: last_data_index.saturating_add(1),
            received_shreds: state.received_shreds,
            recovered_data_shreds: state.recovered_data_shreds,
            fec_sets_recovered: state.fec_sets_recovered,
            entries: decoded.entries,
            transactions: decoded.transactions,
            reconstruction_cpu_ns: state.reconstruction_cpu_ns,
        })
    }

    fn try_recovery(state: &mut SlotAssembly, fec_set_index: u32) {
        let recovered = (|| {
            let fec = state.fec_sets.get_mut(&fec_set_index).unwrap();
            let (num_data, num_code) = fec.erasure_config?;
            let data_count = fec.shreds.values().filter(|shred| shred.is_data()).count();
            let can_attempt = data_count < num_data
                && fec.shreds.len() >= num_data
                && fec.shreds.values().any(WireShred::is_code)
                && fec.last_recovery_attempt_size != fec.shreds.len();
            if !can_attempt {
                return None;
            }
            fec.last_recovery_attempt_size = fec.shreds.len();

            let template = fec.shreds.values().next()?;
            let slot = template.slot;
            let proof_size = template.proof_size;
            let resigned = template.resigned;
            let signature = template.signature;
            let shard_len = template.erasure_shard().len();
            let mut shards = vec![None; num_data.checked_add(num_code)?];
            for shred in fec.shreds.values() {
                if shred.erasure_shard().len() != shard_len {
                    return None;
                }
                let shard_index = match shred.kind {
                    WireKind::Data { .. } => {
                        usize::try_from(shred.index.checked_sub(fec_set_index)?).ok()?
                    }
                    WireKind::Code { position, .. } => num_data.checked_add(position)?,
                };
                let destination = shards.get_mut(shard_index)?;
                if destination.is_some() {
                    return None;
                }
                *destination = Some(shred.erasure_shard().to_vec());
            }

            let started = Instant::now();
            let result = ReedSolomon::new(num_data, num_code)
                .and_then(|reed_solomon| reed_solomon.reconstruct(&mut shards));
            let recovery_cpu_ns = elapsed_ns(started);
            result.ok()?;

            Some((
                shards,
                slot,
                proof_size,
                resigned,
                signature,
                num_data,
                recovery_cpu_ns,
            ))
        })();

        let Some((mut shards, slot, proof_size, resigned, signature, num_data, recovery_cpu_ns)) =
            recovered
        else {
            return;
        };
        state.reconstruction_cpu_ns = state.reconstruction_cpu_ns.saturating_add(recovery_cpu_ns);
        let pending = {
            let fec = state.fec_sets.get(&fec_set_index).unwrap();
            let mut pending = Vec::new();
            for position in 0..num_data {
                let Ok(position_u32) = u32::try_from(position) else {
                    return;
                };
                let Some(index) = fec_set_index.checked_add(position_u32) else {
                    return;
                };
                let key = (false, index);
                if fec.shreds.contains_key(&key) {
                    continue;
                }
                let Some(shard) = shards.get_mut(position).and_then(Option::take) else {
                    return;
                };
                let Some(shred) = WireShred::from_recovered_data(
                    shard,
                    slot,
                    index,
                    fec_set_index,
                    proof_size,
                    resigned,
                    signature,
                ) else {
                    return;
                };
                pending.push((index, key, shred));
            }
            pending
        };
        let Ok(recovered_data) = u32::try_from(pending.len()) else {
            return;
        };
        if recovered_data == 0 {
            return;
        }

        for (index, _, shred) in &pending {
            state.data_indexes.insert(*index);
            if shred.last_in_slot() {
                state.last_data_index = Some(*index);
            }
        }
        let fec = state.fec_sets.get_mut(&fec_set_index).unwrap();
        for (_, key, shred) in pending {
            fec.shreds.insert(key, shred);
        }
        state.recovered_data_shreds = state.recovered_data_shreds.saturating_add(recovered_data);
        if !fec.recovered_data {
            fec.recovered_data = true;
            state.fec_sets_recovered = state.fec_sets_recovered.saturating_add(1);
        }
    }

    fn prune(&mut self, source: SourceId, newest_slot: u64) {
        let oldest = newest_slot.saturating_sub(SLOT_RETENTION);
        self.slots
            .retain(|(slot_source, slot), _| *slot_source != source || *slot >= oldest);
        self.completed
            .retain(|(slot_source, slot)| *slot_source != source || *slot >= oldest);
    }
}

fn validate_and_decode_slot(state: &SlotAssembly, last_index: u32) -> Option<DecodedBlock> {
    let data_shreds: BTreeMap<u32, &WireShred> = state
        .fec_sets
        .values()
        .flat_map(|fec| fec.shreds.values())
        .filter(|shred| shred.is_data())
        .map(|shred| (shred.index, shred))
        .collect();

    let mut batch = Vec::new();
    let mut entries = 0u64;
    let mut transactions = 0u64;
    for index in 0..=last_index {
        let shred = *data_shreds.get(&index)?;
        batch.extend_from_slice(shred.data()?);
        if shred.data_complete() {
            // Agave treats an empty completed data set as an empty entry vector.
            let decoded = if batch.is_empty() {
                DecodedBlock::default()
            } else {
                decode_entry_batch(&batch)?
            };
            entries = entries.saturating_add(decoded.entries);
            transactions = transactions.saturating_add(decoded.transactions);
            batch.clear();
        }
    }

    batch.is_empty().then_some(DecodedBlock {
        entries,
        transactions,
    })
}

/// Validates the Wincode representation used for `Vec<Entry>` without
/// allocating SDK transaction objects. Sequence lengths for entries and
/// transactions use Bincode's fixed-width u64; transaction internals use
/// Solana's canonical short-u16 vectors.
fn decode_entry_batch(bytes: &[u8]) -> Option<DecodedBlock> {
    let mut cursor = WireCursor::new(bytes);
    let entry_count = cursor.bincode_len()?;
    // Every entry has at least num_hashes, hash and transaction-vector length.
    if entry_count > cursor.remaining() / 48 {
        return None;
    }

    let mut transactions = 0u64;
    for _ in 0..entry_count {
        cursor.advance(8 + HASH_SIZE)?;
        let transaction_count = cursor.bincode_len()?;
        // A transaction has at least two short-vector/message discriminator bytes.
        if transaction_count > cursor.remaining() / 2 {
            return None;
        }
        for _ in 0..transaction_count {
            decode_versioned_transaction(&mut cursor)?;
        }
        transactions = transactions.checked_add(u64::try_from(transaction_count).ok()?)?;
    }
    if cursor.remaining() != 0 {
        return None;
    }
    Some(DecodedBlock {
        entries: u64::try_from(entry_count).ok()?,
        transactions,
    })
}

fn decode_versioned_transaction(cursor: &mut WireCursor<'_>) -> Option<()> {
    let signature_count = cursor.short_u16_len()?;
    cursor.advance(signature_count.checked_mul(TRANSACTION_SIGNATURE_SIZE)?)?;

    let discriminator = cursor.byte()?;
    let (versioned, required_signatures) = if discriminator & 0x80 == 0 {
        (false, usize::from(discriminator))
    } else {
        if discriminator != 0x80 {
            return None;
        }
        (true, usize::from(cursor.byte()?))
    };
    if signature_count != required_signatures {
        return None;
    }

    // The legacy discriminator is the first MessageHeader field. V0 read it above.
    cursor.advance(2)?;
    let account_keys = cursor.short_u16_len()?;
    cursor.advance(account_keys.checked_mul(32)?)?;
    cursor.advance(HASH_SIZE)?;

    let instruction_count = cursor.short_u16_len()?;
    for _ in 0..instruction_count {
        cursor.advance(1)?; // program_id_index
        cursor.short_vec_bytes()?; // account indexes
        cursor.short_vec_bytes()?; // instruction data
    }

    if versioned {
        let lookup_count = cursor.short_u16_len()?;
        for _ in 0..lookup_count {
            cursor.advance(32)?; // lookup account key
            cursor.short_vec_bytes()?; // writable indexes
            cursor.short_vec_bytes()?; // readonly indexes
        }
    }
    Some(())
}

struct WireCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> WireCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn advance(&mut self, length: usize) -> Option<()> {
        self.position = self.position.checked_add(length)?;
        (self.position <= self.bytes.len()).then_some(())
    }

    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.position)?;
        self.position += 1;
        Some(byte)
    }

    fn bincode_len(&mut self) -> Option<usize> {
        let value = read_u64(self.bytes, self.position)?;
        self.advance(8)?;
        usize::try_from(value).ok()
    }

    fn short_u16_len(&mut self) -> Option<usize> {
        let mut value = 0usize;
        for index in 0..3 {
            let byte = self.byte()?;
            if index == 2 && byte > 0x03 {
                return None;
            }
            value |= usize::from(byte & 0x7f) << (index * 7);
            if byte & 0x80 == 0 {
                // Reject non-canonical overlong encodings.
                if index > 0 && byte == 0 {
                    return None;
                }
                return Some(value);
            }
        }
        None
    }

    fn short_vec_bytes(&mut self) -> Option<()> {
        let length = self.short_u16_len()?;
        self.advance(length)
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
