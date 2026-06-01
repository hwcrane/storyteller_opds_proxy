use std::{
    fs,
    sync::Arc,
    thread::{self, JoinHandle},
};

use tiny_http::Server;
use ureq::config::Config;

mod config;
mod download;
mod download_queue;
mod http;
mod opds;

use config::ProxyConfig;

fn main() {
    let cfg = Arc::new(ProxyConfig::from_env());
    fs::create_dir_all(&cfg.cache_dir).expect("failed to create cache directory");

    let agent = ureq::Agent::new_with_config(
        Config::builder()
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .build(),
    );

    let server = Arc::new(Server::http(&cfg.listen_addr).expect("Bind failed"));
    eprintln!(
        "opds proxy listening on {} -> {}",
        cfg.listen_addr, cfg.storyteller_url
    );

    let handles: Vec<JoinHandle<()>> = (0..cfg.threads)
        .map(|_| {
            let (server, cfg, agent) = (server.clone(), cfg.clone(), agent.clone());

            thread::spawn(move || {
                while let Ok(req) = server.recv() {
                    if let Err(e) = http::handle(req, &cfg, &agent) {
                        eprintln!("request error: {e}");
                    }
                }
            })
        })
        .collect();

    for h in handles {
        h.join().ok();
    }
}
