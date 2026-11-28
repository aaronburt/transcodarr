# Changelog

Every merge to `main` is released as the next `MAJOR.MINOR` version. Notes for each release, generated from its
pull requests, are on the [releases page](https://github.com/aaronburt/transcodarr/releases).

## [0.1] - 2026-11-28

First release.

- Watch a folder, wait for files to stop changing, and encode them with ffmpeg using TOML profiles of raw ffmpeg args.
- Per-subfolder profiles, `skip_codecs`, `input_args`, `ignore` patterns, `output_name` templates and parallel `jobs`.
- Verified outputs: ffmpeg diagnostics and an ffprobe check before publishing, and duration and stream checks before
  `delete_source` removes an original.
- Originals move to `done/` or `failed/` with a diagnostic log; tool and disk errors retry later.
- Live status page over a WebSocket with progress, retry, config editing and reload, and optional password.
- JSON Lines event log.
- GPU detection with NVENC, QSV and VAAPI encoder listing.
- Docker image with ffmpeg and an optional NVIDIA compose override.
- `run`, `encode`, `profiles` and `--version` commands.
- Release binaries and Docker images built automatically for every merge to `main`.

[0.1]: https://github.com/aaronburt/transcodarr/releases/tag/v0.1
