use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    pub duration_secs: u64,
    pub sources: SourcesConfig,
    pub output: OutputConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct SourcesConfig {
    pub raw_udp: RawUdpConfig,
    pub jito: JitoConfig,
    pub doublezero: DoubleZeroConfig,
    pub yellowstone: YellowstoneConfig,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct RawUdpConfig {
    pub enabled: bool,
    pub bind_addr: String,
    pub recv_buf_size: usize,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct JitoConfig {
    pub enabled: bool,

    // === Direct block engine connection (no proxy needed) ===
    /// e.g. "https://frankfurt.mainnet.block-engine.jito.wtf"
    pub block_engine_url: String,
    /// Path to a Solana keypair JSON file (64-byte array).
    pub auth_keypair_path: String,
    /// Jito regions to subscribe to, e.g. ["frankfurt", "amsterdam"].
    /// Each region is a Jito infrastructure node that forwards shreds from
    /// validators in that geographic area. Use the region(s) nearest your server.
    /// Available: amsterdam, frankfurt, ny, slc, tokyo
    pub desired_regions: Vec<String>,
    /// Your machine's public IP address — Jito will send raw UDP shreds here.
    pub public_ip: String,
    /// Local address to bind for receiving UDP shreds from Jito directly.
    /// e.g. "0.0.0.0:20000"
    pub udp_bind_addr: String,

    // === Already-running jito-shredstream-proxy ===
    /// Bind address matching the proxy's --dest-ip-ports for raw shred forwarding.
    /// e.g. "127.0.0.1:50001"
    pub proxy_udp_addr: String,
    /// Address of the proxy's --grpc-service-port for entry-level streaming.
    /// e.g. "http://127.0.0.1:10001"
    pub proxy_grpc_addr: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct DoubleZeroConfig {
    pub enabled: bool,
    pub multicast_group: String,
    pub port: u16,
    pub interface: String,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct YellowstoneConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub x_token: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct OutputConfig {
    pub log_file: String,
}
