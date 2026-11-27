#!/bin/sh
set -e
[ -e transcodarr.toml ] || cp /defaults/transcodarr.toml .
exec transcodarr run --config transcodarr.toml
