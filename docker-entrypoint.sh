#!/bin/sh
set -eu

DEFAULT_CONFIG="/app/moe-sekai-configs.example.yaml"
PERSISTENT_CONFIG="/data/moe-sekai-configs.yaml"

mkdir -p /data /data/accounts /data/master /data/versions

if [ -z "${CONFIG_PATH:-}" ]; then
  if [ -f "$PERSISTENT_CONFIG" ]; then
    export CONFIG_PATH="$PERSISTENT_CONFIG"
  else
    export CONFIG_PATH="$DEFAULT_CONFIG"
  fi
fi

exec /app/moe-sekai-api
