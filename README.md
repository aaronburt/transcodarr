# Transcodarr

[![CI](https://github.com/aaronburt/transcodarr/actions/workflows/ci.yml/badge.svg)](https://github.com/aaronburt/transcodarr/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/aaronburt/transcodarr)](https://github.com/aaronburt/transcodarr/releases/latest)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

Drop videos in a folder, get them transcoded by ffmpeg. One small binary, no database.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/status-dark.png">
  <img alt="The Transcodarr status page" src="docs/status-light.png">
</picture>

## Docker

```sh
curl -O https://raw.githubusercontent.com/aaronburt/transcodarr/main/compose.yaml
docker compose up -d
```

Put videos in `media/in`, get encodes in `media/out`, and watch at [localhost:8686](http://localhost:8686).
The config is written to `config/transcodarr.toml` on first start.

With an NVIDIA GPU ([Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/) required):

```sh
curl -O https://raw.githubusercontent.com/aaronburt/transcodarr/main/compose.nvidia.yaml
docker compose -f compose.yaml -f compose.nvidia.yaml up -d
```

Update:

```sh
docker compose pull && docker compose up -d
```

## Binary

Download from [releases](https://github.com/aaronburt/transcodarr/releases/latest) (Linux, Windows, macOS). Needs
`ffmpeg` and `ffprobe`.

```sh
transcodarr run                          # watch and encode
transcodarr encode movie.mkv -p h264     # encode one file
transcodarr profiles                     # list profiles
```

## Config

`transcodarr.toml`:

```toml
input = "/media/in"
output = "/media/out"
profile = "hevc"

[web]
host = "127.0.0.1"
port = 8686

[profiles.hevc]
ext = "mkv"
args = ["-map", "0:V", "-map", "0:a?", "-c:v", "libx265", "-crf", "24", "-c:a", "copy"]
skip_codecs = ["hevc", "av1"]
```

A profile is the ffmpeg args that go between the input and the output. More profiles:

```toml
[profiles.h264]
ext = "mp4"
args = ["-map", "0:V", "-map", "0:a?", "-c:v", "libx264", "-crf", "21", "-c:a", "aac", "-b:a", "160k"]

[profiles.av1]
ext = "mkv"
input_args = ["-hwaccel", "cuda"]
args = ["-map", "0:V", "-map", "0:a?", "-c:v", "av1_nvenc", "-preset", "p5", "-cq", "30", "-c:a", "copy"]
```

Give a subfolder its own profile:

```toml
[folders]
anime = "av1"
```

Throw away encodes that look too different from the original:

```toml
[profiles.hevc]
min_vmaf = 93
```

Originals move to `done/` or `failed/` in the input folder. Every option is in the
[reference](docs/reference.md#config) and [`transcodarr.example.toml`](transcodarr.example.toml).

## Security

The status page uses plain HTTP. Editing the config from the page only works on localhost or with a `[web] password`,
because the config can run any program. To use it outside your home network, put it behind a reverse proxy with
HTTPS or a VPN. See the [reference](docs/reference.md#security) for details.

## License

[AGPL-3.0](LICENSE). The Docker image bundles a GPL build of ffmpeg.
