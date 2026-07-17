#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
APP_DIR=${HOME}/.local/share/russian-asr
VENV=${APP_DIR}/venv
PYTHON=${VENV}/bin/python
MODEL_DIR=${APP_DIR}/gigaam-v3-e2e-rnnt
MODEL_ID=ai-sage/GigaAM-v3
MODEL_REVISION=e2e_rnnt

if [[ "${1:-}" == "--print-plan" ]]; then
  echo "model=${MODEL_ID}@${MODEL_REVISION}"
  echo "python=${VENV}"
  echo "model_dir=${MODEL_DIR}"
  echo "files=config.json,modeling_gigaam.py,pytorch_model.bin,tokenizer.model"
  exit 0
fi

if [[ $# -ne 0 ]]; then
  echo "Usage: $0 [--print-plan]" >&2
  exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required. On Debian/Ubuntu: sudo apt install python3 python3-venv" >&2
  exit 1
fi

echo "Preparing an isolated Python environment in $VENV"
mkdir -p "$APP_DIR"
if [[ ! -x "$PYTHON" ]]; then
  python3 -m venv "$VENV"
fi

"$PYTHON" -m pip install --upgrade pip wheel
"$PYTHON" -m pip install \
  --index-url https://download.pytorch.org/whl/cpu \
  'torch==2.10.*' 'torchaudio==2.10.*'
"$PYTHON" -m pip install -r "$ROOT/requirements-model.txt"

echo "Downloading ${MODEL_ID}@${MODEL_REVISION} to $MODEL_DIR"
MODEL_DIR="$MODEL_DIR" MODEL_ID="$MODEL_ID" MODEL_REVISION="$MODEL_REVISION" \
  "$PYTHON" - <<'PY'
import os
from huggingface_hub import snapshot_download

snapshot_download(
    repo_id=os.environ["MODEL_ID"],
    revision=os.environ["MODEL_REVISION"],
    local_dir=os.environ["MODEL_DIR"],
    allow_patterns=[
        "config.json",
        "modeling_gigaam.py",
        "pytorch_model.bin",
        "tokenizer.model",
    ],
)
PY

for file in config.json modeling_gigaam.py pytorch_model.bin tokenizer.model; do
  if [[ ! -s "$MODEL_DIR/$file" ]]; then
    echo "Model download is incomplete: $MODEL_DIR/$file is missing" >&2
    exit 1
  fi
done

echo "GigaAM v3 end-to-end RNN-T is ready."
