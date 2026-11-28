ARG BINARY=source

FROM rust:1-slim-trixie AS source
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY src src
ARG TRANSCODARR_VERSION
RUN cargo build --release --locked && cp target/release/transcodarr /transcodarr

FROM scratch AS prebuilt
ARG TARGETARCH
COPY dist/${TARGETARCH}/transcodarr /transcodarr

FROM ${BINARY} AS binary

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg \
    && rm -rf /var/lib/apt/lists/*
COPY docker/transcodarr.toml /defaults/transcodarr.toml
COPY --chmod=755 docker/start.sh /usr/local/bin/start
COPY --from=binary /transcodarr /usr/local/bin/transcodarr
ENV NVIDIA_DRIVER_CAPABILITIES=compute,video,utility
WORKDIR /config
EXPOSE 8686
CMD ["start"]
