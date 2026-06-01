//! HTTP request routing.
//!
//! This module keeps protocol concerns together: parse the incoming request,
//! forward simple upstream resources, and translate download queue states into
//! HTTP responses KOReader can handle.

use std::{fs::File, io, path::Path};

use tiny_http::{Header, Request, Response};
use url::Url;

use crate::{
    config::ProxyConfig,
    download_queue::{self, DownloadState},
    opds,
};

const DEFAULT_OPDS_CONTENT_TYPE: &str = "application/atom+xml;profile=opds-catalog";
const DEFAULT_BINARY_CONTENT_TYPE: &str = "application/octet-stream";
const EPUB_CONTENT_TYPE: &str = "application/epub+zip";

/// Dispatches one incoming HTTP request to the matching proxy route.
pub fn handle(req: Request, cfg: &ProxyConfig, agent: &ureq::Agent) -> io::Result<()> {
    let target = request_target(&req)?;
    let path = target.path().to_string();
    let auth = get_header(&req, "Authorization");
    let base = public_base(cfg, &req);

    log::debug!("{} {}", req.method(), path);
    match path.as_str() {
        "/health" => todo!(),
        "/" | "/opds" => route_opds(req, cfg, agent, &target, &auth, &base),
        "/fetch" => route_fetch(req, cfg, agent, &target, &auth),
        "/download" => route_download(req, cfg, agent, &target, &auth),
        _ => req.respond(Response::from_string("not found").with_status_code(404)),
    }
}

/// Parses the request target with the URL crate.
fn request_target(req: &Request) -> io::Result<Url> {
    let raw = req.url();
    if let Ok(url) = Url::parse(raw) {
        return Ok(url);
    }

    let base = format!(
        "http://{}",
        get_header(req, "Host").unwrap_or_else(|| "localhost:8088".to_string())
    );
    Url::parse(&base)
        .and_then(|base| base.join(raw))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// Returns a single decoded query value from a parsed request URL.
fn query_param(url: &Url, key: &str) -> Option<String> {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn get_header(req: &Request, field: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(field))
        .map(|h| h.value.as_str().to_string())
}

fn public_base(cfg: &ProxyConfig, req: &Request) -> String {
    if let Some(u) = &cfg.public_url {
        return u.trim_end_matches('/').to_string();
    }
    let host = get_header(req, "Host").unwrap_or_else(|| "localhost:8088".to_string());
    format!("http://{host}")
}

fn header_kv(field: &str, value: &str) -> Header {
    Header::from_bytes(field.as_bytes(), value.as_bytes()).unwrap()
}

fn route_opds(
    req: Request,
    cfg: &ProxyConfig,
    agent: &ureq::Agent,
    target: &Url,
    auth: &Option<String>,
    base: &str,
) -> io::Result<()> {
    let feed_url =
        query_param(target, "u").unwrap_or_else(|| format!("{}/opds", cfg.storyteller_url));
    let mut request = agent.get(&feed_url);
    if let Some(a) = auth {
        request = request.header("Authorization", a);
    }

    match request.call() {
        Ok(mut resp) => {
            let content_type = resp
                .headers()
                .get("Content-Type")
                .and_then(|header| header.to_str().ok())
                .unwrap_or(DEFAULT_OPDS_CONTENT_TYPE)
                .to_string();

            let body = resp
                .body_mut()
                .with_config()
                .limit(cfg.max_body_bytes)
                .read_to_vec()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

            let rewritten = opds::rewrite(&body, &feed_url, base)?;
            let mut out = Response::from_data(rewritten);
            out.add_header(header_kv("Content-Type", &content_type));

            req.respond(out)
        }
        Err(ureq::Error::StatusCode(401)) => {
            req.respond(Response::from_string("unauthorised").with_status_code(401))
        }
        Err(e) => {
            req.respond(Response::from_string(format!("upstream error: {e}")).with_status_code(502))
        }
    }
}

fn route_fetch(
    req: Request,
    cfg: &ProxyConfig,
    agent: &ureq::Agent,
    target: &Url,
    auth: &Option<String>,
) -> io::Result<()> {
    let url = match query_param(target, "u") {
        Some(u) => u,
        None => {
            return req.respond(Response::from_string("missing u").with_status_code(400));
        }
    };

    let mut request = agent.get(&url);
    if let Some(a) = auth {
        request = request.header("Authorization", a.trim());
    }

    match request.call() {
        Ok(mut resp) => {
            let ctype = resp
                .headers()
                .get("Content-Type")
                .and_then(|header| header.to_str().ok())
                .unwrap_or(DEFAULT_BINARY_CONTENT_TYPE)
                .to_string();

            let body = resp
                .body_mut()
                .with_config()
                .limit(cfg.max_body_bytes)
                .read_to_vec()
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

            let mut out = Response::from_data(body);
            out.add_header(header_kv("Content-Type", &ctype));

            req.respond(out)
        }
        Err(ureq::Error::StatusCode(401)) => {
            req.respond(Response::from_string("unauthorised").with_status_code(401))
        }
        Err(e) => {
            req.respond(Response::from_string(format!("upstream error: {e}")).with_status_code(502))
        }
    }
}

fn route_download(
    req: Request,
    cfg: &ProxyConfig,
    agent: &ureq::Agent,
    target: &Url,
    auth: &Option<String>,
) -> io::Result<()> {
    let url = match query_param(target, "u") {
        Some(u) => u,
        None => {
            return req.respond(Response::from_string("missing u").with_status_code(400));
        }
    };

    match download_queue::start_or_get(&url, auth, cfg, agent.clone()) {
        DownloadState::Ready {
            path,
            delete_after_open,
        } => respond_epub_file(req, &path, delete_after_open),
        DownloadState::Preparing => respond_processing(req),
        DownloadState::Failed(error) => req.respond(
            Response::from_string(format!("epub rewrite error: {error}")).with_status_code(502),
        ),
    }
}

fn respond_epub_file(req: Request, path: &Path, delete_after_open: bool) -> io::Result<()> {
    let file = File::open(path)?;
    if delete_after_open {
        let _ = std::fs::remove_file(path);
    }

    let mut out = Response::from_file(file);
    out.add_header(header_kv("Content-Type", EPUB_CONTENT_TYPE));
    out.add_header(header_kv(
        "Content-Disposition",
        "attachment; filename=\"readaloud-stripped.epub\"",
    ));

    req.respond(out)
}

fn respond_processing(req: Request) -> io::Result<()> {
    let mut out = Response::from_string("processing\n").with_status_code(202);
    out.add_header(header_kv("Retry-After", "10"));
    req.respond(out)
}
