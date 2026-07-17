#!/usr/bin/env python3
"""Persistent JSON-lines worker for the local GigaAM model."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import importlib.util

MAX_CHUNK_SECONDS = 22.0
DEFAULT_MODEL = Path.home() / ".local/share/russian-asr/gigaam-v3-e2e-rnnt"


def model_loader_kind(model_path: Path) -> str:
    if model_path.name == "gigaam-v3-e2e-rnnt" or (
        model_path / "tokenizer.model"
    ).is_file():
        return "local-e2e-rnnt"
    return "transformers"


def audio_duration(path: Path) -> float:
    result = subprocess.run(
        [
            "ffprobe", "-v", "error", "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1", str(path),
        ],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return float(result.stdout.strip())


def split_audio(path: Path, output_dir: Path) -> list[Path]:
    pattern = output_dir / "chunk-%04d.wav"
    subprocess.run(
        [
            "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
            "-i", str(path), "-ar", "16000", "-ac", "1", "-f", "segment",
            "-segment_time", str(MAX_CHUNK_SECONDS), "-c:a", "pcm_s16le",
            str(pattern),
        ],
        check=True,
    )
    return sorted(output_dir.glob("chunk-*.wav"))


def load_model():
    import torch

    model_path = Path(os.environ.get("GIGATYPE_MODEL", DEFAULT_MODEL)).expanduser()
    if not (model_path / "pytorch_model.bin").is_file():
        raise FileNotFoundError(f"GigaAM model is missing from {model_path}")
    torch.set_num_threads(max(1, min(8, os.cpu_count() or 1)))

    if model_loader_kind(model_path) == "local-e2e-rnnt":
        from omegaconf import OmegaConf

        spec = importlib.util.spec_from_file_location(
            "modeling_gigaam", model_path / "modeling_gigaam.py"
        )
        if spec is None or spec.loader is None:
            raise RuntimeError("could not load the local GigaAM v3 model code")
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)

        config = json.loads((model_path / "config.json").read_text())
        cfg = OmegaConf.create(config["cfg"]["model"]["cfg"])
        cfg.decoding.model_path = str(model_path / "tokenizer.model")
        model = module.GigaAMASR(cfg)
        state = torch.load(
            model_path / "pytorch_model.bin", map_location="cpu", weights_only=True
        )
        state = {key.removeprefix("model."): value for key, value in state.items()}
        missing, unexpected = model.load_state_dict(state)
        if missing or unexpected:
            raise RuntimeError(
                f"GigaAM v3 weights do not match: missing={missing}, unexpected={unexpected}"
            )
        model.eval()
        return model

    from transformers import AutoModel

    model = AutoModel.from_pretrained(
        str(model_path), trust_remote_code=True, local_files_only=True
    )
    model.eval()
    return model


def transcribe(model, path: Path) -> str:
    if audio_duration(path) <= 25.0:
        return str(model.transcribe(str(path))).strip()
    transcripts: list[str] = []
    with tempfile.TemporaryDirectory(prefix="gigatype-chunks-") as tmp:
        for chunk in split_audio(path, Path(tmp)):
            text = str(model.transcribe(str(chunk))).strip()
            if text:
                transcripts.append(text)
    return " ".join(transcripts)


def main() -> int:
    if sys.argv[1:] == ["--print-model-plan"]:
        model_path = Path(os.environ.get("GIGATYPE_MODEL", DEFAULT_MODEL)).expanduser()
        print(f"model={model_path}")
        print(f"loader={model_loader_kind(model_path)}")
        return 0

    fake = os.environ.get("GIGATYPE_FAKE_TRANSCRIPT")
    model = None if fake is not None else load_model()
    for line in sys.stdin:
        if not line.strip():
            continue
        request_id = None
        try:
            request = json.loads(line)
            request_id = request.get("id")
            audio_path = Path(request["audio_path"])
            text = fake if fake is not None else transcribe(model, audio_path)
            reply = {"id": request_id, "text": text}
        except Exception as exc:
            reply = {"id": request_id, "error": str(exc)}
        print(json.dumps(reply, ensure_ascii=False), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
