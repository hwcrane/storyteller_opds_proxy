use std::io::{self, Cursor, Write};

use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use url::Url;

/// Rewrites all OPDS link targets in an XML document.
pub fn rewrite(input: &[u8], feed_url: &str, proxy_base: &str) -> io::Result<Vec<u8>> {
    let base_feed = Url::parse(feed_url).map_err(invalid_url)?;
    let proxy_base = Url::parse(proxy_base).map_err(invalid_url)?;

    let mut reader = Reader::from_reader(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) if is_entry(&e) => {
                let entry = collect_element(&mut reader, e.clone().into_owned(), &mut buf)?;
                if should_keep_entry(&entry, &base_feed)? {
                    let rewritten = rewrite_links(&entry, &base_feed, &proxy_base)?;
                    writer.get_mut().write_all(&rewritten)?;
                }
            }
            Ok(Event::Start(e)) if is_link(&e) => {
                if should_drop_link(&e, &base_feed) {
                    let name = e.name().as_ref().to_vec();
                    skip_element(&mut reader, &name, &mut buf)?;
                } else {
                    let rewritten = rewrite_link(&e, &base_feed, &proxy_base)?;
                    writer
                        .write_event(Event::Start(rewritten))
                        .map_err(xml_err)?;
                }
            }
            Ok(Event::Empty(e)) if is_link(&e) => {
                if !should_drop_link(&e, &base_feed) {
                    let rewritten = rewrite_link(&e, &base_feed, &proxy_base)?;
                    writer
                        .write_event(Event::Empty(rewritten))
                        .map_err(xml_err)?;
                }
            }
            Ok(e) => {
                writer.write_event(e).map_err(xml_err)?;
            }
            Err(e) => {
                return Err(xml_err(e));
            }
        }

        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

fn rewrite_links(input: &[u8], feed_url: &Url, proxy_base: &Url) -> io::Result<Vec<u8>> {
    let mut reader = Reader::from_reader(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) if is_link(&e) => {
                if should_drop_link(&e, feed_url) {
                    let name = e.name().as_ref().to_vec();
                    skip_element(&mut reader, &name, &mut buf)?;
                } else {
                    let rewritten = rewrite_link(&e, feed_url, proxy_base)?;
                    writer
                        .write_event(Event::Start(rewritten))
                        .map_err(xml_err)?;
                }
            }
            Ok(Event::Empty(e)) if is_link(&e) => {
                if !should_drop_link(&e, feed_url) {
                    let rewritten = rewrite_link(&e, feed_url, proxy_base)?;
                    writer
                        .write_event(Event::Empty(rewritten))
                        .map_err(xml_err)?;
                }
            }
            Ok(e) => {
                writer.write_event(e).map_err(xml_err)?;
            }
            Err(e) => return Err(xml_err(e)),
        }

        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

fn rewrite_link(
    e: &BytesStart,
    feed_url: &Url,
    proxy_base: &Url,
) -> io::Result<BytesStart<'static>> {
    let Some(href) = attr_value(e, b"href") else {
        return Ok(e.clone().into_owned());
    };

    let rel = attr_value(e, b"rel").unwrap_or_default();
    let typ = attr_value(e, b"type").unwrap_or_default();
    let is_readaloud = href_has_readaloud_format(feed_url, &href)?;
    let new_href = routed_url(&href, &rel, &typ, is_readaloud, feed_url, proxy_base)?;

    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);

    for a in e.attributes().flatten() {
        let key = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        if key == "href" {
            out.push_attribute(("href", new_href.as_str()));
        } else {
            let value = String::from_utf8_lossy(&a.value).into_owned();
            out.push_attribute((key.as_str(), value.as_str()));
        }
    }

    Ok(out)
}

fn should_drop_link(e: &BytesStart, feed_url: &Url) -> bool {
    let rel = attr_value(e, b"rel").unwrap_or_default();
    let href = attr_value(e, b"href");

    is_acquisition(&rel)
        && !href
            .as_deref()
            .and_then(|href| href_has_readaloud_format(feed_url, href).ok())
            .unwrap_or(false)
}

fn attr_value(e: &BytesStart, key: &[u8]) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.as_ref() == key {
            return Some(String::from_utf8_lossy(&a.value).into_owned());
        }
    }
    None
}

fn routed_url(
    href: &str,
    rel: &str,
    typ: &str,
    is_readaloud: bool,
    feed_url: &Url,
    proxy_base: &Url,
) -> io::Result<String> {
    let upstream_url = feed_url.join(href).map_err(invalid_url)?;
    let proxy_path = if typ.contains("application/atom+xml") {
        "/opds"
    } else if is_acquisition(rel) && is_readaloud {
        "/download"
    } else {
        "/fetch"
    };

    proxied_url(proxy_base, proxy_path, upstream_url.as_str())
}

/// Builds a proxy URL with `u=` using the URL crate's form encoding rules.
fn proxied_url(proxy_base: &Url, path: &str, upstream_url: &str) -> io::Result<String> {
    let mut url = proxy_base.join(path).map_err(invalid_url)?;
    url.query_pairs_mut().append_pair("u", upstream_url);
    Ok(url.to_string())
}

fn is_link(e: &BytesStart<'_>) -> bool {
    e.local_name().as_ref() == b"link"
}

fn is_entry(e: &BytesStart<'_>) -> bool {
    e.local_name().as_ref() == b"entry"
}

fn is_acquisition(rel: &str) -> bool {
    rel.contains("acquisition")
}

fn href_has_readaloud_format(feed_url: &Url, href: &str) -> io::Result<bool> {
    let href = feed_url.join(href).map_err(invalid_url)?;
    Ok(href
        .query_pairs()
        .any(|(key, value)| key == "format" && value == "readaloud"))
}

fn should_keep_entry(entry: &[u8], feed_url: &Url) -> io::Result<bool> {
    let mut reader = Reader::from_reader(entry);
    let mut buf = Vec::new();
    let mut has_acquisition = false;
    let mut has_readaloud = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if is_link(&e) => {
                let rel = attr_value(&e, b"rel").unwrap_or_default();
                if is_acquisition(&rel) {
                    has_acquisition = true;
                    if attr_value(&e, b"href")
                        .as_deref()
                        .map(|href| href_has_readaloud_format(feed_url, href))
                        .transpose()?
                        .unwrap_or(false)
                    {
                        has_readaloud = true;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => return Err(xml_err(e)),
        }

        buf.clear();
    }

    Ok(!has_acquisition || has_readaloud)
}

fn collect_element<R: std::io::BufRead>(
    reader: &mut Reader<R>,
    start: BytesStart<'static>,
    buf: &mut Vec<u8>,
) -> io::Result<Vec<u8>> {
    let name = start.name().as_ref().to_vec();
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    writer.write_event(Event::Start(start)).map_err(xml_err)?;

    let mut depth = 1;
    while depth > 0 {
        buf.clear();
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == name {
                    depth += 1;
                }
                writer
                    .write_event(Event::Start(e.into_owned()))
                    .map_err(xml_err)?;
            }
            Ok(Event::Empty(e)) => {
                writer
                    .write_event(Event::Empty(e.into_owned()))
                    .map_err(xml_err)?;
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == name {
                    depth -= 1;
                }
                writer
                    .write_event(Event::End(e.into_owned()))
                    .map_err(xml_err)?;
            }
            Ok(Event::Eof) => break,
            Ok(e) => {
                writer.write_event(e.into_owned()).map_err(xml_err)?;
            }
            Err(e) => return Err(xml_err(e)),
        }
    }

    Ok(writer.into_inner().into_inner())
}

fn skip_element<R: std::io::BufRead>(
    reader: &mut Reader<R>,
    name: &[u8],
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    let mut depth = 1;

    while depth > 0 {
        buf.clear();
        match reader.read_event_into(buf) {
            Ok(Event::Start(e)) if e.name().as_ref() == name => depth += 1,
            Ok(Event::End(e)) if e.name().as_ref() == name => depth -= 1,
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(xml_err(e)),
        }
    }

    Ok(())
}

fn invalid_url(e: url::ParseError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, e)
}

fn xml_err<E>(e: E) -> io::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    io::Error::new(io::ErrorKind::InvalidData, e)
}
