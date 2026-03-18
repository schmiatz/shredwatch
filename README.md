# shredwatch

A Solana shred source latency benchmark. Connects to multiple shred sources simultaneously, tracks nanosecond-precision first-arrival times per `(slot, index, shred_type)` tuple, and prints comparison tables showing which source is fastest, most reliable, and cheapest in overhead.

---

## What the sources measure

### Raw UDP — direct Turbine

You bind a port on your machine and Solana's Turbine protocol delivers shreds directly to you, the same way validators receive them. You need to be in the TVU/shred gossip path — typically you configure a validator or a jito-shredstream-proxy alternative to forward Turbine shreds to your port, or you run a validator node yourself.

**What it measures:** The raw speed of the Solana gossip/Turbine network delivering shreds to your IP.

---

### Jito ShredStream — two sub-modes

**Direct block engine** (no proxy needed): Your machine authenticates with Jito's block engine gRPC, sends periodic heartbeats saying "send shreds to my IP:port", and Jito delivers raw UDP shreds directly to you. These are the same binary format as Turbine shreds.

**Proxy UDP:** You run `jito-shredstream-proxy` locally. It handles auth and heartbeats, then forwards the raw shreds it receives to `127.0.0.1:yourport`. Your machine just listens on that local port.

Both modes produce identical data — raw Solana shreds. The difference is only in who manages the auth. Since shreds arrive via UDP from Jito's infrastructure rather than directly from validators via Turbine, this shows you how much latency Jito's relay adds vs direct Turbine.

**What it measures:** Jito's relay overhead on top of raw Turbine — are you getting shreds faster or slower through Jito's network than through direct Turbine?

---

### DoubleZero — multicast

DoubleZero is a private fiber/multicast network for Solana infrastructure. Shreds are multicasted over their network and arrive as raw UDP on a multicast group. You need to be subscribed on-chain and have the `doublezerod` daemon running. The shred binary format is identical to Turbine.

**What it measures:** Whether DoubleZero's private network delivers shreds faster than public internet paths (Turbine) and Jito's relay.

---

### Yellowstone gRPC — different granularity entirely

Yellowstone is a Geyser plugin running inside a validator. It does not expose raw shreds. Instead:

- `SLOT_FIRST_SHRED_RECEIVED` event: fires when the validator's TVU first receives the opening shred of a new slot
- `SubscribeDeshred`: fires when shreds have been reassembled into entries, before execution

**What it measures:** Not shred latency, but validator processing latency — how long after the first raw shred arrived on your other sources does Yellowstone signal that the validator has the data. This is inherently slower and at slot granularity, not shred granularity. Shown in a separate table.

---

## Output tables

### Latency table — Raw UDP, Jito, DoubleZero

Of all shreds that arrived on this source, how many nanoseconds/microseconds after the globally-first arrival did they come in?

> If Raw UDP has p50 = 0 µs and Jito has p50 = 55 µs, it means Jito's relay consistently adds ~55 µs at the median.

### First arrival wins

Which source received each shred first, as a percentage:

```
72% Raw UDP   25% DoubleZero   3% Jito
```

This is the most direct answer to "which source is fastest." Raw UDP winning 72% of shreds means Turbine usually beats Jito's relay. But DoubleZero winning 25% means for some validators, DoubleZero's private network is faster.

### Coverage

What fraction of all observed shreds did each source actually receive:

```
Raw UDP 99.8%   Jito 99.3%
```

Shows reliability — Jito might miss some shreds, or your Turbine peer might drop some.

### Dupes

How many duplicate deliveries each source sends. Jito intentionally sends shreds multiple times for reliability. High dupe counts waste bandwidth but improve coverage.

### Slot/entry latency table — Yellowstone only

```
p50 = 3.1 ms after first raw shred
```

How long after the network delivered the first shred does the validator's Geyser plugin notify you. Reflects deshredding time, not network latency.

---

## Questions it answers

| Question | How the tool answers it |
|---|---|
| Is Jito faster than raw Turbine? | Win % + latency table comparing Raw UDP vs Jito |
| Does DoubleZero's private network beat public internet? | Win % + p99 comparing DoubleZero vs Raw UDP |
| How reliable is each source? | Coverage % and missed counts |
| How much overhead does Jito's relay add? | p50/p99 delta between Jito and Raw UDP |
| When do I have the data as a Yellowstone subscriber? | Slot latency table — ms after first shred |
| Which source should I use for a latency-sensitive strategy? | Win % is the direct answer |

---

## Limitations

- Does **not** measure absolute wall-clock latency — only relative latency between sources on the **same machine**
- If you only enable one source, the latency table shows all zeros (nothing to compare against)
- Yellowstone comparison is not apples-to-apples with the UDP sources — different data granularity
- Cannot compare sources on different machines (no clock sync)

The tool is most useful when you run **at least two UDP-level sources simultaneously** so the latency deltas are meaningful.

---

## Installation

Requires Rust (stable). Clone and build:

```bash
git clone https://github.com/schmiatz/shredwatch
cd shredwatch
cargo build --release
```

The binary will be at `target/release/shredwatch`.

---

## Configuration

Copy `config.example.toml` to `config.toml` and edit:

```bash
cp config.example.toml config.toml
```

Enable the sources you want to compare. You need at least two for meaningful results.

### Jito direct mode (recommended — no proxy needed)

```toml
[sources.jito]
enabled = true
block_engine_url   = "https://frankfurt.mainnet.block-engine.jito.wtf"
auth_keypair_path  = "/path/to/your-keypair.json"
desired_regions    = ["frankfurt"]
public_ip          = "YOUR_SERVER_PUBLIC_IP"
udp_bind_addr      = "0.0.0.0:20000"
proxy_udp_addr     = ""
proxy_grpc_addr    = ""
```

> **Important:** `public_ip` must be your server's actual public IP — Jito sends UDP shreds to that address. Port 20000 (or whatever you set) must be open in your firewall.

### Raw UDP (Turbine direct)

Requires being in the shred gossip path or having a validator forward shreds to you:

```toml
[sources.raw_udp]
enabled = true
bind_addr = "0.0.0.0:8001"
recv_buf_size = 10485760
```

---

## Usage

```bash
# Run with default config.toml
./target/release/shredwatch

# Custom config and duration
./target/release/shredwatch --config my.toml --duration 60

# With JSON-lines log file
./target/release/shredwatch --log-file shredwatch.log
```

**Options:**

```
-c, --config <FILE>    Path to TOML config file [default: config.toml]
-d, --duration <SECS>  Benchmark duration in seconds (overrides config)
-l, --log-file <FILE>  Log file path (overrides config)
    --no-log           Disable log file output
```

---

## Sample output

```
╔══════════════════════════════════════════════════════════════════════════════════════════╗
║              SHRED BENCHMARK  ·  30s  ·  87,412 unique shreds  ·  2 sources              ║
║                          started 2025-06-01 14:22:03 UTC                                  ║
╚══════════════════════════════════════════════════════════════════════════════════════════╝

LATENCY RELATIVE TO FIRST ARRIVAL  (per shred, data + FEC combined)
┌──────────────────┬──────────┬─────────┬─────────┬─────────┬─────────┬─────────┬──────────┐
│ Source           │ Shreds   │  p50    │  p90    │  p95    │  p99    │  p99.9  │   max    │
├──────────────────┼──────────┼─────────┼─────────┼─────────┼─────────┼─────────┼──────────┤
│ Raw UDP          │ 86,901   │  0 µs   │  0 µs   │  0 µs   │  0 µs   │  12 µs  │  1.8 ms  │
│ Jito ShredStream │ 85,234   │ 55.2 µs │ 98.4 µs │ 142 µs  │ 891 µs  │  8.1 ms │ 92.3 ms  │
└──────────────────┴──────────┴─────────┴─────────┴─────────┴─────────┴─────────┴──────────┘

FIRST ARRIVAL WINS  (which source received each shred first)
┌──────────────────┬───────────┬──────────┐
│ Source           │ Won First │ Win Rate │
├──────────────────┼───────────┼──────────┤
│ Raw UDP          │ 63,847    │ 73.2%    │
│ Jito ShredStream │ 23,403    │ 26.8%    │
└──────────────────┴───────────┴──────────┘

COVERAGE & RELIABILITY
┌──────────────────┬──────────┬──────────┬───────┬────────┐
│ Source           │ Received │ Coverage │ Dupes │ Missed │
├──────────────────┼──────────┼──────────┼───────┼────────┤
│ Raw UDP          │ 86,901   │  99.4%   │ 0     │ 511    │
│ Jito ShredStream │ 85,234   │  97.5%   │ 891   │ 2,178  │
└──────────────────┴──────────┴──────────┴───────┴────────┘

  Total unique shreds: 87,412  ·  Total slots observed: 72
```
