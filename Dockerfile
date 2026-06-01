FROM rust:1-bookworm AS build

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim

WORKDIR /app
COPY --from=build /app/target/release/storyteller_opds_proxy /usr/local/bin/storyteller_opds_proxy

ENV LISTEN_ADDR=0.0.0.0:8088 \
    CACHE_DIR=/cache

EXPOSE 8088
CMD ["storyteller_opds_proxy"]
