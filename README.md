# Transcodarr

[![CI](https://github.com/aaronburt/transcodarr/actions/workflows/ci.yml/badge.svg)](https://github.com/aaronburt/transcodarr/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/aaronburt/transcodarr)](https://github.com/aaronburt/transcodarr/releases/latest)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)

Watch a folder and transcode new videos with ffmpeg. One small binary, no database: the folders are the state.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/status-dark.png">
  <img alt="The Transcodarr status page, showing a file encoding, files waiting, recent results and profiles" src="docs/status-light.png">
</picture>

- Drop videos in a folder and get encodes out the other side. Originals move to `done/` or `failed/`.
- A profile is just ffmpeg arguments, so any encoder ffmpeg has works, including NVENC, QSV and VAAPI.
- Subfolders can use their own profile, such as `anime/` to AV1.
- A live status page with progress, retry and a config editor that checks your changes before saving.
- Every event can go to a JSON Lines log for scripts.
- No database and only three runtime dependencies. A restart just rescans.

## Quick start

### Docker

```sh
mkdir transcodarr && cd transcodarr
curl -O https://raw.githubusercontent.com/aaronburt/transcodarr/main/compose.yaml
docker compose up -d
```

On first start it writes `config/transcodarr.toml` (default profile `hevc`, libx265) and serves the status page on
[localhost:8686](http://localhost:8686). Put videos in `media/in`; encodes land in `media/out`.

The image runs as root, so its files are owned by root. To keep your own ownership, add `user: "1000:1000"` (your
uid and gid) to the service and make sure `config/` and `media/` belong to that user.

The image is [distroless](https://github.com/GoogleContainerTools/distroless): just Transcodarr, a self-contained ffmpeg and
ffprobe 9.0 from [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) (x264, x265, SVT-AV1 and NVENC), and
glibc. There is no shell or package manager inside it. To run ffmpeg in it, use `docker run --entrypoint ffmpeg ...`.

It encodes on the CPU by default, so it runs on any machine. To use an NVIDIA card, install the NVIDIA Container
Toolkit on the host, start with the GPU passed in, and pick the `av1` profile (or any NVENC profile) in the config:

```sh
curl -O https://raw.githubusercontent.com/aaronburt/transcodarr/main/compose.nvidia.yaml
docker compose -f compose.yaml -f compose.nvidia.yaml up -d
```

The page and the startup log show the GPU that was found and the encoder each profile uses. If an NVENC profile runs
without a GPU, the error says so and the file stays queued.

Images are published to `ghcr.io/aaronburt/transcodarr` for `linux/amd64` and `linux/arm64`, tagged with each
version (such as `0.1`) and `latest`.

### Binary

Download the archive for your system from the [latest release](https://github.com/aaronburt/transcodarr/releases/latest)
(Linux x86_64 and arm64, Windows x86_64, macOS Apple silicon). It needs `ffmpeg` and `ffprobe` on the `PATH`, or
set in the config.

```sh
cp transcodarr.example.toml transcodarr.toml
transcodarr run                          # watch and encode; status page per [web]
transcodarr encode movie.mkv -p h264     # encode one file now
transcodarr profiles                     # list profiles
```

Edit `input`, `output` and `profile` in `transcodarr.toml` first. The Linux builds are static and run on any distro.

### From source

Needs Rust 1.89 or newer.

```sh
git clone https://github.com/aaronburt/transcodarr
cd transcodarr
cargo build --release
./target/release/transcodarr --help
```

## How it works


1. The watcher checks ffmpeg and ffprobe at startup. It scans every `idle_secs` (default 60) when idle. A new video must keep the same size and modification time for at least `scan_secs` (default 10). A producer that may pause mid-copy can write under a dot-prefixed name and rename the finished file into place.
2. The watcher reserves the output and moves the input into `.transcodarr-processing/` inside its watched folder. A new arrival with the same name stays in the inbox. Each watched folder supports one active watcher.
3. Both `run` and `encode` use a unique temporary output under the output folder's `.transcodarr/`. They check ffmpeg's diagnostics and probe the result before publishing it. An existing output is left in place.
4. Successful originals move to `done/`. With `delete_source = true`, deletion also requires verified container, video, and audio durations and at least as many streams of each type as the original. A substantial duration shortfall fails verification. A smaller shortfall beyond timestamp rounding, an unknown duration, or a duration-changing profile keeps the original in `done/`.
5. Damaged inputs move to `failed/` with a diagnostic log. A missing tool at startup stops the watcher before it claims any inputs. Tool failures during a job and output-storage errors leave the job queued for retry. A claimed file stays in `.transcodarr-processing/` until the worker can finish it.

`jobs` files encode at once (default 1). A restart picks up claimed files. If the output already exists for an interrupted job, the original moves to `done/` even when deletion is enabled. Name collisions in `done/` and `failed/` use numbered prefixes.

## Config

| Key | Default | What |
| --- | --- | --- |
| `input`, `output` | required | the watched folder and where encodes go |
| `profile` | required | the profile `run` uses, and `encode` unless `-p` is given |
| `scan_secs` | 10 | scan interval while a file is still being copied |
| `idle_secs` | 60 | scan interval when nothing is waiting |
| `extensions` | `mp4 mkv mov avi m4v ts webm` | which files count as videos |
| `ignore` | none | file name patterns to leave alone, with `*` and `?`, case-insensitive, e.g. `["*.part", "sample*"]` |
| `output_name` | `{name}` | output file name before the extension; `{name}` is the input's name, `{profile}` the profile's, e.g. `"{name} [{profile}]"` |
| `jobs` | 1 | how many encodes run at once; 1 suits CPU encoding, NVENC handles 2–3 |
| `ffmpeg`, `ffprobe` | `ffmpeg`, `ffprobe` | paths to the binaries, if they aren't on the `PATH` |
| `done_dir`, `failed_dir` | `done/`, `failed/` in each watched folder | where originals go after encoding |
| `delete_source` | false | delete originals after a full-length encode passes duration and stream-count checks |
| `log_file` | none | append every event to this file as one JSON object per line, for scripts or an AI to read |
| `[folders]` | none | subfolders of `input` with their own profile, e.g. `anime = "av1"`; their encodes go to `output/anime/` |
| `[web]` `enabled`, `host`, `port` | off, `127.0.0.1`, 8686 | the status page; a `[web]` section turns it on, and `--web` overrides it |
| `[web]` `password` | none | asks for this password (any username) before showing the page |

Set `host = "0.0.0.0"` to reach the page from other machines, and read [Security](#security) first.

`output_name` must contain `{name}`. ffmpeg is started directly, not through a shell, so shell quoting
doesn't matter. Characters Windows doesn't allow in file names (`<>:"/\|?*`) become `_`, as does a leading `-`.
The file always lands inside the output folder.

## Web page

<img alt="The config editor on the status page" src="docs/config-editor.png" width="720">

The page shows what's encoding, waiting and recently finished, live over a WebSocket at `/ws` (the same JSON is
at `GET /api/status`). It can also:

- **Edit the config:** the whole `transcodarr.toml` in a text box. Saving checks it first and never writes a bad
  config, then applies it straight away. Changes to `[web]` `host` and `port` need a restart.
- **Reload the config:** with a button, or on its own when the file changes on disk (noticed on the next scan).
  A broken file is reported on the page and the old config keeps running.
- **Retry a failed file:** moves it from `failed/` back into its watched folder.

## Security

The status page is meant for your own machine or home network. Read this before exposing it anywhere else.

- **Editing needs localhost or a password.** The config can name any program as `ffmpeg`, so whoever can edit it
  can run commands as Transcodarr's user. Without a `password` the page is read-only unless you open it on
  localhost. In Docker the request comes from the Docker network, not localhost, so set a `password` to edit there.
- **The password is sent in the clear.** It uses HTTP Basic auth over plain HTTP, so anyone on the network path can
  read it. Use a password you don't use anywhere else. To reach the page from outside your network, put it behind a
  reverse proxy with HTTPS (Caddy, nginx, Traefik) or a VPN such as Tailscale, never a bare port forward.
- **The password is stored in plain text** in `transcodarr.toml`. Keep that file readable only by you.
- **Edits must come from the page itself.** Requests from other websites open in your browser are refused, so a
  malicious page can't change your config through your browser.

To report a security problem, open a [private advisory](https://github.com/aaronburt/transcodarr/security/advisories/new)
rather than a public issue.

## Profiles

A profile is the ffmpeg args between `-i <input>` and the output file, plus the output extension, so GPU
encoders (`hevc_nvenc`, `hevc_qsv`, ...) need nothing special. See `transcodarr.example.toml`.

Each profile can also have `input_args`, ffmpeg args that go before `-i`, such as `["-hwaccel", "cuda"]` to decode
on the GPU as well as encode there.

Each profile can also have `skip_codecs`, e.g. `["av1"]`: a file whose video is already in one of those codecs (as
ffprobe names them) isn't re-encoded, and its original goes straight to done.

By default a decode error stops the encode and the file moves to failed. Set `ignore_errors = true` on a profile to
encode through glitches instead, which suits recordings like `.ts` captures. The output is still checked with ffprobe,
so a file that ends far too short still fails.

Profiles default to `preserve_duration = true`. Set it to `false` on profiles that intentionally trim or speed up a
video. Transcodarr then accepts the changed duration and keeps the source. The example profiles map every audio
track. The Matroska profiles also map subtitles and attachments.

## Event log

With `log_file` set, `run` appends one JSON object per line. Each has `time` (Unix seconds) and `event`:

| Event | Fields |
| --- | --- |
| `started` | `watches` (folder, profile and output paths), `jobs`, `scan_secs`, `idle_secs` |
| `encode_started` | `file`, `input`, `output`, `profile`, `input_args`, `args`, and `source` (what ffprobe found) |
| `finished` | `result` (`done`, `failed` or `skipped`), sizes, `took_secs`, `note`, `input`, `output`, and `ffmpeg_output` (the last 40 lines of ffmpeg's output) when it failed |
| `queued_for_retry` | `file`, `error` (a tool or disk problem, tried again later) |
| `config_reloaded`, `config_error` | `error` when the new config was rejected |
| `retry_requested` | `file`, when Retry was pressed on the web page |


## Code

| File | What |
| --- | --- |
| `main.rs` | argument parsing and the three commands |
| `config.rs` | the TOML config and profiles |
| `scan.rs` | the watch loop and done/failed moves |
| `encode.rs` | running ffmpeg and reading its progress |
| `status.rs` | state shared by the loop and the page: live config, reload, retry, status |
| `web.rs`, `index.html` | the web page and its actions, on `std::net` |
| `ws.rs` | the minimal WebSocket handshake and frames (SHA-1 and base64 included) |

Runtime dependencies are `serde`, `toml` and `serde_json`; `tempfile` is only used by the tests.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

The watcher tests need `ffmpeg` and `ffprobe` on the `PATH`, with the libx264 and libx265 encoders. CI runs all of
this on every push and pull request.

Every merge to `main` that passes CI is released automatically. The version is `MAJOR.MINOR`, and the minor number
goes up by one each release (`v0.1`, `v0.2`, ...). The release workflow builds the binaries, attaches them to a GitHub
release with notes generated from the merged pull requests, and pushes the `0.2` and `latest` images. To start a
new major version, push a tag such as `v1.0` on `main`; the next merges become `v1.1`, `v1.2` and so on.

## License

[AGPL-3.0](LICENSE). You can use, change and share Transcodarr freely. If you share a modified version, or run one
for other people over a network, you have to make its source available under the same license.

The Docker image also contains ffmpeg and ffprobe built by BtbN/FFmpeg-Builds under the GPL. The exact build is
pinned in the `Dockerfile`, and its source is the matching FFmpeg commit together with the build scripts in that
repository.
