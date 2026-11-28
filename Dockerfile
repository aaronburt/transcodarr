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

FROM --platform=$BUILDPLATFORM debian:trixie-slim AS ffmpeg
ARG TARGETARCH
ARG FFMPEG_RELEASE=autobuild-2026-11-27-12-42
ARG FFMPEG_BUILD=ffmpeg-n9.0.2-982-g836df4e858
ARG FFMPEG_SHA256_AMD64=a236601b679237dece06c8dbf1662cdb138f197c8109c9fa4ae5f0e884d61ffb
ARG FFMPEG_SHA256_ARM64=c384a7a2bc6268a51b0a4d2d79ac647cd5e0360379f13db563646262dd3525da
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
    && rm -rf /var/lib/apt/lists/*
RUN case "$TARGETARCH" in \
        amd64) arch=linux64; sum=$FFMPEG_SHA256_AMD64 ;; \
        arm64) arch=linuxarm64; sum=$FFMPEG_SHA256_ARM64 ;; \
        *) echo "no ffmpeg build for $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o ffmpeg.tar.xz "https://github.com/BtbN/FFmpeg-Builds/releases/download/$FFMPEG_RELEASE/$FFMPEG_BUILD-$arch-gpl-9.0.tar.xz" \
    && echo "$sum  ffmpeg.tar.xz" | sha256sum -c - \
    && mkdir /out \
    && tar -xJf ffmpeg.tar.xz -C /out --strip-components=2 --wildcards '*/bin/ffmpeg' '*/bin/ffprobe' \
    && rm ffmpeg.tar.xz

FROM gcr.io/distroless/cc-debian13
COPY --from=ffmpeg /out/ /usr/local/bin/
COPY --from=binary /transcodarr /usr/local/bin/transcodarr
COPY docker/transcodarr.toml /defaults/transcodarr.toml
ENV NVIDIA_DRIVER_CAPABILITIES=compute,video,utility
WORKDIR /config
EXPOSE 8686
ENTRYPOINT ["transcodarr"]
CMD ["run", "--config", "/config/transcodarr.toml", "--default-config", "/defaults/transcodarr.toml"]
