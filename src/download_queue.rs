use std::{
    collections::HashMap,
    fs::{self, File},
    hash::{Hash, Hasher},
    io::{self, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{config::ProxyConfig, download};

/// Result of asking the download queue for a stripped EPUB.
pub enum DownloadState {
    /// A stripped EPUB exists and can be served immediately.
    Ready {
        path: PathBuf,
        delete_after_open: bool,
    },
    /// A background worker is preparing the stripped EPUB.
    Preparing,
    /// The previous preparation attempt failed.
    Failed(String),
}

#[derive(Clone)]
enum JobStatus {
    Preparing,
    Failed(String),
}

/// Returns the current download state, starting a background preparation job
/// when no fresh cache entry or job exists.
pub fn start_or_get(
    url: &str,
    auth: &Option<String>,
    cfg: &ProxyConfig,
    agent: ureq::Agent,
) -> DownloadState {
    let cache_path = cache_path(cfg, url, auth);
    if cache_is_fresh(&cache_path, cfg.cache_ttl_secs) {
        return DownloadState::Ready {
            path: cache_path,
            delete_after_open: cfg.cache_ttl_secs == 0,
        };
    }

    let key = cache_key(url, auth);
    match job_status(&key) {
        Some(JobStatus::Preparing) => return DownloadState::Preparing,
        Some(JobStatus::Failed(error)) => {
            clear_job(&key);
            return DownloadState::Failed(error);
        }
        None => {}
    }

    mark_job_preparing(key.clone());
    spawn_prepare_job(
        agent,
        url.to_string(),
        auth.clone(),
        cfg.max_body_bytes,
        cache_path,
        key,
    );
    DownloadState::Preparing
}

fn spawn_prepare_job(
    agent: ureq::Agent,
    url: String,
    auth: Option<String>,
    max_body_bytes: u64,
    cache_path: PathBuf,
    job_key: String,
) {
    thread::spawn(move || {
        let result =
            prepare_download_file(agent, &url, auth.as_deref(), max_body_bytes, &cache_path);
        match result {
            Ok(()) => clear_job(&job_key),
            Err(e) => mark_job_failed(job_key, e.to_string()),
        }
    });
}

fn prepare_download_file(
    agent: ureq::Agent,
    url: &str,
    auth: Option<&str>,
    max_body_bytes: u64,
    cache_path: &PathBuf,
) -> io::Result<()> {
    let mut request = agent.get(url);
    if let Some(a) = auth {
        request = request.header("Authorization", a.trim());
    }

    let mut resp = request
        .call()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("upstream error: {e}")))?;

    let input_tmp = unique_tmp_path(cache_path, "download");
    let output_tmp = unique_tmp_path(cache_path, "stripped");

    let result = (|| -> io::Result<()> {
        let mut input_file = File::create(&input_tmp)?;
        let mut upstream = resp.body_mut().with_config().limit(max_body_bytes).reader();
        io::copy(&mut upstream, &mut input_file)?;
        input_file.flush()?;
        drop(input_file);

        // Zip archives need seek access to the central directory, so the
        // upstream EPUB is staged on disk before the file-to-file rewrite.
        download::strip_audio_for_kindle_file(&input_tmp, &output_tmp)?;
        fs::rename(&output_tmp, cache_path)?;
        Ok(())
    })();

    let _ = fs::remove_file(&input_tmp);
    let _ = fs::remove_file(&output_tmp);
    result
}

fn cache_path(cfg: &ProxyConfig, url: &str, auth: &Option<String>) -> PathBuf {
    cfg.cache_dir.join(format!("{}.epub", cache_key(url, auth)))
}

fn cache_is_fresh(path: &PathBuf, ttl_secs: u64) -> bool {
    if !path.exists() {
        return false;
    }

    // `CACHE_TTL_SECS=0` means "serve once": the ready file must remain visible
    // long enough for KOReader's retry request, then http.rs unlinks it after
    // opening the response file handle.
    if ttl_secs == 0 {
        return true;
    }

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(age) = modified.elapsed() else {
        return false;
    };

    if age.as_secs() <= ttl_secs {
        true
    } else {
        let _ = fs::remove_file(path);
        false
    }
}

fn cache_key(url: &str, auth: &Option<String>) -> String {
    let mut key = Fnv1a64::default();
    url.hash(&mut key);
    auth.as_deref().unwrap_or("").hash(&mut key);
    format!("{:016x}", key.finish())
}

fn unique_tmp_path(path: &PathBuf, label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_extension(format!("{label}.{nonce}.tmp"))
}

fn jobs() -> &'static Mutex<HashMap<String, JobStatus>> {
    static JOBS: OnceLock<Mutex<HashMap<String, JobStatus>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn job_status(key: &str) -> Option<JobStatus> {
    jobs().lock().ok().and_then(|jobs| jobs.get(key).cloned())
}

fn mark_job_preparing(key: String) {
    if let Ok(mut jobs) = jobs().lock() {
        jobs.insert(key, JobStatus::Preparing);
    }
}

fn mark_job_failed(key: String, error: String) {
    if let Ok(mut jobs) = jobs().lock() {
        jobs.insert(key, JobStatus::Failed(error));
    }
}

fn clear_job(key: &str) {
    if let Ok(mut jobs) = jobs().lock() {
        jobs.remove(key);
    }
}

#[derive(Default)]
struct Fnv1a64(u64);

impl Hasher for Fnv1a64 {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = 0xcbf29ce484222325;
        }

        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}
