# Storyteller OPDS Proxy

An OPDS 1.x proxy for using [Storyteller](https://storyteller-platform.dev/) generated readaloud EPUBs with e-readers and other OPDS clients.

It was designed for KOReader running on a Kindle, but it should work with other OPDS 1.x clients that can browse a catalog and download EPUB acquisition links.

## Why This Exists

Storyteller produces readaloud EPUBs after forced alignment. Those EPUBs are useful even if you do not want the audio, because the forced-alignment step injects metadata that can be used for progress syncing.

The problem is size and compatibility:

- Readaloud EPUBs can be very large because they include audio files
- Kindle import workflows do not need the audio payload
- Storyteller serves both text-only and aligned EPUBs via OPDS
- Many OPDS clients do not clearly show which acquisition format is being selected
- Clients should not be offered incompatible non-readaloud formats

This proxy sits between an OPDS client and Storyteller. It exposes an OPDS catalog that only shows books with a Storyteller `format=readaloud` acquisition, then prepares a smaller EPUB by removing audio and media-overlay files while preserving the XHTML content.

It is designed to be lightweight, I've measured memory usage to be around 5MB at idle and up to 10MB while processing a book.

## What It Does

For OPDS feeds, the proxy:

- Forwards requests to Storyteller
- Filters book entries that do not have a `format=readaloud` acquisition link
- Removes non-readaloud acquisition links from mixed entries
- Rewrites OPDS links so the client continues through the proxy

For EPUB downloads, the proxy:

- Downloads the Storyteller readaloud EPUB
- Removes audio files from the EPUB zip
- Removes SMIL/media-overlay files
- Rewrites only the OPF package document to remove removed manifest items and `media-overlay` attributes
- Leaves XHTML files untouched
- Serves the stripped EPUB

## Download Flow

Large readaloud EPUBs can be 1-2GB or more. OPDS clients may time out if the proxy tries to download, strip, and return the EPUB in one blocking request.

To avoid that, `/download` is retryable:

1. The OPDS client requests a book.
2. If the stripped EPUB is not cached, the proxy starts a background preparation job and returns `202 processing`.
3. The client can retry the same download. If the download is still processing, `202 processing` is returned again.
4. Once preparation is complete, the same `/download` URL returns the stripped EPUB.

To avoid a buildup of files, or to pick up a new version after realignment, cache expiry is controlled with `CACHE_TTL_SECS`. Set it to `0` to serve each prepared EPUB once and delete it after the successful response is opened.

## Deployment

The easiest deployment is to run the proxy in the same Docker Compose project as Storyteller. In that setup, set `STORYTELLER_URL` to Storyteller's Compose service name:

```yaml
STORYTELLER_URL: "http://storyteller:8001"
```

Set `PUBLIC_URL` to the URL your OPDS client uses to reach the proxy:

```yaml
PUBLIC_URL: "http://your-server:8088"
```


Example Compose file:

```yaml
services:
  storyteller-opds-proxy:
    image: ghcr.io/hwcrane/storyteller_opds_proxy:latest
    restart: unless-stopped
    depends_on:
      - web # Remove this if Storyteller is not in this Compose file.
    ports:
      - "8088:8088"
    environment:
      STORYTELLER_URL: "http://web:8001"
      PUBLIC_URL: "http://your-server:8088"
      LISTEN_ADDR: "0.0.0.0:8088"
      CACHE_DIR: "/cache"
      CACHE_TTL_SECS: "0" # 0 auto removes once served
      MAX_BODY_BYTES: "5368709120"
      THREADS: "4"
      RUST_LOG: "info"
    volumes:
      - opds_proxy_cache:/cache

volumes:
  opds_proxy_cache:
```

If Storyteller is outside this Compose project, remove the optional `web`, `secrets`, and `storyteller_data` parts, remove `depends_on`, and set `STORYTELLER_URL` to a URL reachable from inside the proxy container.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `STORYTELLER_URL` | `http://localhost:8001` | Upstream Storyteller URL. |
| `PUBLIC_URL` | request host | External URL used when rewriting OPDS links. |
| `LISTEN_ADDR` | `0.0.0.0:8088` | Address the proxy listens on. |
| `CACHE_DIR` | `./cache` | Directory for temporary and stripped EPUB files. |
| `CACHE_TTL_SECS` | `86400` | How long stripped EPUBs are reused. `0` means serve once, then delete. |
| `MAX_BODY_BYTES` | `5368709120` | Maximum upstream body size to read. Default is 5GB. |
| `THREADS` | `4` | Number of blocking HTTP worker threads. |
| `RUST_LOG` | `info` | Logging filter, for example `debug` or `storyteller_opds_proxy=debug`. |
