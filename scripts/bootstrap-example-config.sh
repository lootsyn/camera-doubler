#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
copy_if_missing() {
  local src="$1" dst="$2"
  if [[ ! -e "$dst" ]]; then cp "$src" "$dst"; echo "created $dst"; else echo "kept existing $dst"; fi
}
copy_if_missing .env.edge.example .env.edge
copy_if_missing .env.receiver.example .env.receiver
copy_if_missing .env.dataset-builder.example .env.dataset-builder
copy_if_missing .env.adapter-rby1.example .env.adapter-rby1
copy_if_missing config/camera-policy.example.yaml config/camera-policy.yaml
copy_if_missing config/embodiment.example.yaml config/embodiment.yaml
