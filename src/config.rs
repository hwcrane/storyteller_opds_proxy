use std::{env, path::PathBuf};

/// Settings shared by all request handler threads.
///
/// * `storyteller_url`: Base URL of the upstream Storyteller service.
/// * `public_url`: Externally visible URL for this proxy, used when rewriting OPDS links.
/// * `cache_dir`: Cache directory reserved for download processing.
/// * `listen_addr`: TCP address accepted by `tiny_http`, defaults to `0.0.0.0:8088`.
/// * `threads`: Number of blocking request handler threads.
/// * `max_body_bytes`: Maximum upstream response body size to read.
/// * `cache_ttl_secs`: How long stripped EPUBs remain reusable. Zero means serve once.
pub struct ProxyConfig {
    pub storyteller_url: String,
    pub public_url: Option<String>,
    pub cache_dir: PathBuf,
    pub listen_addr: String,
    pub threads: usize,
    pub max_body_bytes: u64,
    pub cache_ttl_secs: u64,
}

impl ProxyConfig {
    /// Builds configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            storyteller_url: env::var("STORYTELLER_URL")
                .unwrap_or_else(|_| "http://localhost:8001".to_string())
                .trim_end_matches('/')
                .to_string(),
            public_url: env::var("PUBLIC_URL").ok(),
            cache_dir: PathBuf::from(
                env::var("CACHE_DIR").unwrap_or_else(|_| "./cache".to_string()),
            ),
            listen_addr: env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8088".to_string()),
            threads: env::var("THREADS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(4),
            max_body_bytes: env::var("MAX_BODY_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5 * 1024 * 1024 * 1024),
            cache_ttl_secs: env::var("CACHE_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(24 * 60 * 60),
        }
    }
}
