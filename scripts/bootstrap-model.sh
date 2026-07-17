#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
MODEL_DIR=${GIGATYPE_MODEL:-${HOME}/.local/share/gigatype/models/gigaam-v3-e2e-rnnt}
MODEL_NAME=GigaType/gigaam-v3-e2e-rnnt-onnx
MODEL_RELEASE=${GIGATYPE_MODEL_RELEASE:-model-v3-e2e-rnnt}
MODEL_BASE_URL=${GIGATYPE_MODEL_BASE_URL:-https://github.com/kpoxo6op/gigatype/releases/download/${MODEL_RELEASE}}
FILES=(
  v3_e2e_rnnt_encoder.onnx
  v3_e2e_rnnt_decoder.onnx
  v3_e2e_rnnt_joint.onnx
  tokenizer.model
)

if [[ "${1:-}" == "--print-plan" ]]; then
  echo "model=${MODEL_NAME}"
  echo "release=${MODEL_RELEASE}"
  echo "model_dir=${MODEL_DIR}"
  echo "files=$(IFS=,; echo "${FILES[*]}")"
  exit 0
fi

if [[ $# -ne 0 ]]; then
  echo "Usage: $0 [--print-plan]" >&2
  exit 2
fi

for command in curl sha256sum; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "$command is required to install the model" >&2
    exit 1
  fi
done

mkdir -p "$MODEL_DIR"
tmp=$(mktemp -d "${MODEL_DIR}.download.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

for file in "${FILES[@]}"; do
  if [[ -s "$MODEL_DIR/$file" ]]; then
    continue
  fi
  echo "Downloading $file"
  curl --fail --location --retry 3 --continue-at - \
    --output "$tmp/$file" "$MODEL_BASE_URL/$file"
done

while read -r expected file; do
  if [[ -f "$tmp/$file" ]]; then
    actual=$(sha256sum "$tmp/$file" | cut -d' ' -f1)
  elif [[ -f "$MODEL_DIR/$file" ]]; then
    actual=$(sha256sum "$MODEL_DIR/$file" | cut -d' ' -f1)
  else
    echo "Model download is incomplete: $file is missing" >&2
    exit 1
  fi
  if [[ "$actual" != "$expected" ]]; then
    echo "Checksum mismatch for $file" >&2
    exit 1
  fi
done <"$ROOT/packaging/model-sha256.txt"

for file in "${FILES[@]}"; do
  [[ -f "$tmp/$file" ]] && mv "$tmp/$file" "$MODEL_DIR/$file"
done
chmod 0644 "$MODEL_DIR"/*
echo "GigaAM v3 end-to-end RNN-T ONNX is ready in $MODEL_DIR"
