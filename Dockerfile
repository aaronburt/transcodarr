FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY src src
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/transcodarr /usr/local/bin/transcodarr
COPY docker/transcodarr.toml /defaults/transcodarr.toml
COPY --chmod=755 docker/start.sh /usr/local/bin/start
ENV NVIDIA_DRIVER_CAPABILITIES=compute,video,utility
WORKDIR /config
EXPOSE 8686
CMD ["start"]
