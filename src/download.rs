use std::{
    collections::HashSet,
    fs::File,
    io::{self, Cursor, Read, Write},
    path::Path,
};

use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub fn strip_audio_for_kindle_file(input: &Path, output: &Path) -> io::Result<()> {
    let input = File::open(input)?;
    let mut archive = ZipArchive::new(input).map_err(zip_err)?;
    let mut output = File::create(output)?;
    strip_archive(&mut archive, &mut output)
}

fn strip_archive<R, W>(archive: &mut ZipArchive<R>, output: &mut W) -> io::Result<()>
where
    R: Read + io::Seek,
    W: Write + io::Seek,
{
    validate_epub(archive)?;

    let opf_path = find_opf_path(archive)?;
    let opf = read_named_file(archive, &opf_path)?;
    let removed_paths = removable_manifest_paths(&opf, &opf_path)?;
    let rewritten_opf = rewrite_opf(&opf)?;

    rebuild_epub(archive, output, &opf_path, &rewritten_opf, &removed_paths)
}

fn validate_epub<R: Read + io::Seek>(archive: &mut ZipArchive<R>) -> io::Result<()> {
    let mimetype = read_named_file(archive, "mimetype")?;
    if mimetype == b"application/epub+zip" {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zip is not an EPUB",
        ))
    }
}

fn find_opf_path<R: Read + io::Seek>(archive: &mut ZipArchive<R>) -> io::Result<String> {
    let container = read_named_file(archive, "META-INF/container.xml")?;
    let mut reader = Reader::from_reader(container.as_slice());
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.local_name().as_ref() == b"rootfile" => {
                if let Some(path) = attr_value(&e, b"full-path") {
                    return Ok(path);
                }
            }
            Ok(_) => {}
            Err(e) => return Err(xml_err(e)),
        }

        buf.clear();
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "container.xml does not identify an OPF package",
    ))
}

fn read_named_file<R: Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
) -> io::Result<Vec<u8>> {
    let mut file = archive.by_name(name).map_err(zip_err)?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

fn removable_manifest_paths(opf: &[u8], opf_path: &str) -> io::Result<HashSet<String>> {
    let mut reader = Reader::from_reader(opf);
    let mut buf = Vec::new();
    let mut removed = HashSet::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if is_manifest_item(&e) => {
                let href = attr_value(&e, b"href").unwrap_or_default();
                let media_type = attr_value(&e, b"media-type").unwrap_or_default();
                if should_remove_manifest_item(&href, &media_type) {
                    removed.insert(resolve_opf_href(opf_path, &href));
                }
            }
            Ok(_) => {}
            Err(e) => return Err(xml_err(e)),
        }

        buf.clear();
    }

    Ok(removed)
}

fn rewrite_opf(opf: &[u8]) -> io::Result<Vec<u8>> {
    let mut reader = Reader::from_reader(opf);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) if is_manifest_item(&e) => {
                if should_remove_manifest_item_event(&e) {
                    let name = e.name().as_ref().to_vec();
                    skip_element(&mut reader, &name, &mut buf)?;
                } else {
                    writer
                        .write_event(Event::Start(strip_media_overlay_attr(&e)))
                        .map_err(xml_err)?;
                }
            }
            Ok(Event::Empty(e)) if is_manifest_item(&e) => {
                if !should_remove_manifest_item_event(&e) {
                    writer
                        .write_event(Event::Empty(strip_media_overlay_attr(&e)))
                        .map_err(xml_err)?;
                }
            }
            Ok(e) => writer.write_event(e).map_err(xml_err)?,
            Err(e) => return Err(xml_err(e)),
        }

        buf.clear();
    }

    Ok(writer.into_inner().into_inner())
}

fn rebuild_epub<R: Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    output: &mut (impl Write + io::Seek),
    opf_path: &str,
    rewritten_opf: &[u8],
    removed_paths: &HashSet<String>,
) -> io::Result<()> {
    let mut writer = ZipWriter::new(output);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    // EPUB requires `mimetype` to be the first entry and stored without compression.
    writer.start_file("mimetype", stored).map_err(zip_err)?;
    writer.write_all(b"application/epub+zip")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(zip_err)?;
        let name = file.name().to_string();

        if name == "mimetype" || removed_paths.contains(&normalize_zip_path(&name)) {
            continue;
        }

        if file.is_dir() {
            writer.add_directory(name, deflated).map_err(zip_err)?;
            continue;
        }

        writer.start_file(&name, deflated).map_err(zip_err)?;
        if normalize_zip_path(&name) == normalize_zip_path(opf_path) {
            writer.write_all(rewritten_opf)?;
        } else {
            io::copy(&mut file, &mut writer)?;
        }
    }

    writer.finish().map_err(zip_err)?;
    Ok(())
}

fn should_remove_manifest_item_event(e: &BytesStart) -> bool {
    let href = attr_value(e, b"href").unwrap_or_default();
    let media_type = attr_value(e, b"media-type").unwrap_or_default();
    should_remove_manifest_item(&href, &media_type)
}

fn should_remove_manifest_item(href: &str, media_type: &str) -> bool {
    let media_type = media_type.to_ascii_lowercase();
    media_type.starts_with("audio/")
        || media_type == "application/smil+xml"
        || has_audio_extension(href)
        || extension_is(href, "smil")
}

fn has_audio_extension(path: &str) -> bool {
    ["mp3", "m4a", "aac", "ogg", "opus", "wav", "flac"]
        .iter()
        .any(|ext| extension_is(path, ext))
}

fn extension_is(path: &str, ext: &str) -> bool {
    path.rsplit(['/', '?', '#'])
        .next()
        .and_then(|file| file.rsplit_once('.'))
        .map(|(_, actual)| actual.eq_ignore_ascii_case(ext))
        .unwrap_or(false)
}

fn strip_media_overlay_attr(e: &BytesStart) -> BytesStart<'static> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut out = BytesStart::new(name);

    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == b"media-overlay" {
            continue;
        }

        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = String::from_utf8_lossy(&attr.value).into_owned();
        out.push_attribute((key.as_str(), value.as_str()));
    }

    out
}

fn attr_value(e: &BytesStart, key: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == key {
            return Some(String::from_utf8_lossy(&attr.value).into_owned());
        }
    }
    None
}

fn is_manifest_item(e: &BytesStart<'_>) -> bool {
    e.local_name().as_ref() == b"item"
}

fn resolve_opf_href(opf_path: &str, href: &str) -> String {
    let href = href.split(['?', '#']).next().unwrap_or(href);
    let base = opf_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    let joined = if href.starts_with('/') {
        href.trim_start_matches('/').to_string()
    } else if base.is_empty() {
        href.to_string()
    } else {
        format!("{base}/{href}")
    };
    normalize_zip_path(&joined)
}

fn normalize_zip_path(path: &str) -> String {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

fn skip_element<R: io::BufRead>(
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

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

fn xml_err<E>(e: E) -> io::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    io::Error::new(io::ErrorKind::InvalidData, e)
}
