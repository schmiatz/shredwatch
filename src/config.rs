use serde::Deserialize;

fn default_silence_timeout() -> u64 {
    30
}

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    /// Run for this many seconds. Set to 0 when using start_slot/end_slot.
    pub duration_secs: u64,
    /// Only record shreds at or after this slot (optional).
    #[serde(default)]
    pub start_slot: Option<u64>,
    /// Stop the run once a shred beyond this slot is seen (optional).
    #[serde(default)]
    pub end_slot: Option<u64>,
    /// Stop if no shreds are received for this many seconds (default: 30).
    #[serde(default = "default_silence_timeout")]
    pub silence_timeout_secs: u64,
    pub sources: SourcesConfig,
    pub output: OutputConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct SourcesConfig {
    #[serde(default)]
    pub raw_udp: Vec<RawUdpConfig>,
    #[serde(default)]
    pub jito: Vec<JitoConfig>,
    #[serde(default)]
    pub doublezero: Vec<DoubleZeroConfig>,
    #[serde(default)]
    pub yellowstone: Vec<YellowstoneConfig>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct RawUdpConfig {
    /// Display name used in result tables and log files.
    #[serde(default)]
    pub name: String,
    pub enabled: bool,
    pub bind_addr: String,
    pub recv_buf_size: usize,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct JitoConfig {
    /// Display name used in result tables and log files.
    #[serde(default)]
    pub name: String,
    pub enabled: bool,

    // === Direct block engine connection (no proxy needed) ===
    /// e.g. "https://frankfurt.mainnet.block-engine.jito.wtf"
    pub block_engine_url: String,
    /// Path to a Solana keypair JSON file (64-byte array).
    pub auth_keypair_path: String,
    /// Jito regions to subscribe to, e.g. ["frankfurt", "amsterdam"].
    pub desired_regions: Vec<String>,
    /// Your machine's public IP address — Jito will send raw UDP shreds here.
    pub public_ip: String,
    /// Local address to bind for receiving Jito's direct UDP shreds.
    pub udp_bind_addr: String,

    // === Already-running jito-shredstream-proxy ===
    /// Bind address matching the proxy's --dest-ip-ports for raw shred forwarding.
    pub proxy_udp_addr: String,
    /// Address of the proxy's --grpc-service-port for entry-level streaming.
    pub proxy_grpc_addr: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DoubleZeroConfig {
    /// Display name used in result tables and log files.
    #[serde(default)]
    pub name: String,
    pub enabled: bool,
    pub multicast_group: String,
    pub port: u16,
    pub interface: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct YellowstoneConfig {
    /// Display name used in result tables and log files.
    #[serde(default)]
    pub name: String,
    pub enabled: bool,
    pub endpoint: String,
    pub x_token: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct OutputConfig {
    pub log_file: String,
}
