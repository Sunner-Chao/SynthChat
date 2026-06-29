import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import unicodedata
import wave
from pathlib import Path


def _default_model_dir() -> Path:
    for key in (
        "SYNTHCHAT_CHATTTS_MODEL_DIR",
        "SYNTHCHAT_TTS_MODEL_DIR",
        "HERMES_CHATTTS_MODEL_DIR",
        "HERMES_TTS_MODEL_DIR",
        "CHAT_TTS_MODEL_DIR",
        "CHATTTS_MODEL_DIR",
    ):
        value = os.environ.get(key, "").strip()
        if value:
            return Path(value).expanduser()

    candidates: list[Path] = []
    roots: list[Path] = []
    try:
        roots.append(Path.cwd().resolve())
    except Exception:
        roots.append(Path.cwd())
    try:
        roots.extend(Path(__file__).resolve().parents)
    except Exception:
        pass
    for root in roots:
        candidates.append(root / "models" / "ChatTTS")
        candidates.append(root / "ChatTTS")
    seen: set[str] = set()
    for candidate in candidates:
        key = str(candidate).lower()
        if key in seen:
            continue
        seen.add(key)
        if candidate.exists():
            return candidate
    return Path("models") / "ChatTTS"


DEFAULT_MODEL_DIR = _default_model_dir()

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    sys.stderr.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass


def _fail(message: str, code: int = 2) -> None:
    print(json.dumps({"ok": False, "error": message}, ensure_ascii=True), file=sys.stderr)
    raise SystemExit(code)


def _sanitize_tts_text(text: str) -> str:
    normalized = unicodedata.normalize("NFKC", text or "")
    replacements = {
        "～": ",",
        "~": ",",
        "？": "?",
        "！": "!",
        "。": ".",
        "，": ",",
        "、": ",",
        "；": ";",
        "：": ":",
        "“": '"',
        "”": '"',
        "‘": "'",
        "’": "'",
        "（": "(",
        "）": ")",
        "《": " ",
        "》": " ",
        "【": " ",
        "】": " ",
        "…": "...",
        "—": "-",
        "·": " ",
    }
    normalized = "".join(replacements.get(ch, ch) for ch in normalized)
    normalized = re.sub(r"[\(\[\{][^\)\]\}]{0,32}?(?:笑|眨|抬头|低头|轻轻|语气|动作|表情)[^\)\]\}]{0,32}?[\)\]\}]", " ", normalized)
    normalized = re.sub(r"https?://\S+", " ", normalized)
    kept: list[str] = []
    for ch in normalized:
        if ch == "\ufffd" or unicodedata.category(ch) in {"Cc", "Cs"}:
            continue
        code = ord(ch)
        is_cjk = (
            0x3400 <= code <= 0x4DBF
            or 0x4E00 <= code <= 0x9FFF
            or 0xF900 <= code <= 0xFAFF
        )
        if is_cjk or ch.isascii() and (ch.isalnum() or ch.isspace() or ch in ".,!?;:'\"()-_%"):
            kept.append(ch)
            continue
        if unicodedata.category(ch).startswith(("S", "P")):
            kept.append(" ")
    cleaned = "".join(kept)
    cleaned = re.sub(r"\s+", " ", cleaned)
    cleaned = re.sub(r"([,.!?;:])\1+", r"\1", cleaned)
    return cleaned.strip(" ,;:")


def _load_speaker_embedding(value: str, torch_module):
    speaker_embedding = (value or "").strip() or None
    if not speaker_embedding:
        return None
    path = Path(speaker_embedding)
    if not path.is_file():
        if path.suffix.lower() in {".pt", ".pth", ".safetensors"}:
            print(
                json.dumps(
                    {
                        "ok": True,
                        "warning": f"speaker embedding file not found, falling back to speaker seed: {path}",
                    },
                    ensure_ascii=True,
                ),
                file=sys.stderr,
            )
            return None
        return speaker_embedding
    try:
        loaded = torch_module.load(path, map_location="cpu")
    except Exception as exc:
        _fail(f"failed to load speaker embedding file: {path}: {exc}")
    if isinstance(loaded, str):
        return loaded
    if isinstance(loaded, dict):
        for key in ("speaker_embedding", "spk_emb", "embedding", "speaker"):
            item = loaded.get(key)
            if isinstance(item, str):
                return item
        _fail(f"speaker embedding file has no supported string field: {path}")
    return loaded


def _write_wav(path: Path, pcm, sample_rate: int) -> float:
    try:
        import numpy as np
    except Exception as exc:
        _fail(f"numpy is required for ChatTTS output: {exc}")
    data = np.asarray(pcm)
    if data.ndim > 1:
        data = data.reshape(-1)
    if data.dtype != np.int16:
        data = np.clip(data, -1.0, 1.0)
        data = (data * 32767.0).astype(np.int16)
    with wave.open(str(path), "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(sample_rate)
        out.writeframes(data.tobytes())
    return len(data) * 1000.0 / max(1, sample_rate)


def _write_mp3(wav_path: Path) -> Path | None:
    ffmpeg = shutil.which("ffmpeg")
    if not ffmpeg:
        return None
    mp3_path = wav_path.with_suffix(".mp3")
    command = [
        ffmpeg,
        "-y",
        "-loglevel",
        "error",
        "-i",
        str(wav_path),
        "-codec:a",
        "libmp3lame",
        "-b:a",
        "48k",
        str(mp3_path),
    ]
    try:
        subprocess.run(command, check=True)
    except Exception:
        return None
    return mp3_path if mp3_path.is_file() else None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--text", default="")
    parser.add_argument("--text-file", default="")
    parser.add_argument("--out", required=True)
    parser.add_argument("--sample-rate", type=int, default=16000)
    parser.add_argument(
        "--model-dir",
        default=os.environ.get("SYNTHCHAT_CHATTTS_MODEL_DIR", str(DEFAULT_MODEL_DIR)),
    )
    parser.add_argument("--speed", type=int, default=5)
    parser.add_argument("--oral", type=int, default=2)
    parser.add_argument("--laugh", type=int, default=0)
    parser.add_argument("--break-level", type=int, default=4)
    parser.add_argument("--speaker-seed", type=int, default=0)
    parser.add_argument("--speaker-embedding", default="")
    parser.add_argument("--temperature", type=float, default=0.3)
    parser.add_argument("--top-p", type=float, default=0.7)
    parser.add_argument("--top-k", type=int, default=20)
    parser.add_argument("--refine-text", action=argparse.BooleanOptionalAction, default=True)
    parser.add_argument("--refine-prompt", default="")
    parser.add_argument("--refine-temperature", type=float, default=0.7)
    parser.add_argument(
        "--silk",
        action=argparse.BooleanOptionalAction,
        default=False,
        help="Also encode the generated WAV to WeChat SILK at --out.",
    )
    args = parser.parse_args()

    raw_text = args.text
    if args.text_file:
        raw_text = Path(args.text_file).read_text(encoding="utf-8")
    text = _sanitize_tts_text(raw_text)
    if not text:
        _fail("text is empty")
    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    wav_path = out_path.with_suffix(".wav")

    try:
        import ChatTTS
    except Exception as exc:
        _fail(f"ChatTTS is not installed: {exc}")

    try:
        import torch
        import torchaudio
    except Exception as exc:
        _fail(f"torch and torchaudio are required by ChatTTS: {exc}")

    try:
        chat = ChatTTS.Chat()
        model_dir = Path(args.model_dir)
        if model_dir.exists():
            loaded = chat.load(source="custom", custom_path=str(model_dir), compile=False)
        else:
            _fail(f"ChatTTS model directory does not exist: {model_dir}")
        if not loaded:
            _fail("ChatTTS model load returned false")
        speed = max(1, min(9, int(args.speed)))
        oral = max(0, min(9, int(args.oral)))
        laugh = max(0, min(9, int(args.laugh)))
        break_level = max(0, min(9, int(args.break_level)))
        infer_prompt = f"[speed_{speed}]"
        refine_prompt = args.refine_prompt.strip() or f"[oral_{oral}][laugh_{laugh}][break_{break_level}]"
        speaker_embedding = _load_speaker_embedding(args.speaker_embedding, torch)
        if speaker_embedding is None and args.speaker_seed:
            torch.manual_seed(int(args.speaker_seed))
            speaker_embedding = chat.sample_random_speaker()
        params_refine = ChatTTS.Chat.RefineTextParams(
            prompt=refine_prompt,
            temperature=max(0.01, float(args.refine_temperature)),
            show_tqdm=False,
        )
        params_infer = ChatTTS.Chat.InferCodeParams(
            prompt=infer_prompt,
            spk_emb=speaker_embedding,
            temperature=max(0.01, float(args.temperature)),
            top_P=max(0.01, min(1.0, float(args.top_p))),
            top_K=max(1, int(args.top_k)),
            show_tqdm=False,
        )
        wavs = chat.infer(
            [text],
            skip_refine_text=not args.refine_text,
            params_refine_text=params_refine,
            params_infer_code=params_infer,
        )
        if not wavs:
            _fail("ChatTTS returned no audio")
        wav = wavs[0]
        source_rate = int(getattr(chat, "sample_rate", 24000) or 24000)
        if args.sample_rate and args.sample_rate != source_rate:
            tensor = torch.tensor(wav).float()
            if tensor.ndim == 1:
                tensor = tensor.unsqueeze(0)
            resampler = torchaudio.transforms.Resample(source_rate, args.sample_rate)
            wav = resampler(tensor).squeeze(0).cpu().numpy()
            sample_rate = args.sample_rate
        else:
            sample_rate = source_rate
        playtime_ms = int(round(_write_wav(wav_path, wav, sample_rate)))
    except SystemExit:
        raise
    except Exception as exc:
        _fail(f"ChatTTS synthesis failed: {exc}")

    if args.silk:
        try:
            from graiax import silkcoder
            silkcoder.encode(str(wav_path), str(out_path), rate=sample_rate, tencent=True)
        except Exception as exc:
            _fail(f"SILK encoding failed: {exc}")
    else:
        if out_path.suffix.lower() == ".wav":
            if out_path != wav_path:
                shutil.copyfile(wav_path, out_path)
        else:
            mp3_candidate = _write_mp3(wav_path)
            if mp3_candidate:
                if mp3_candidate.resolve() != out_path.resolve():
                    shutil.copyfile(mp3_candidate, out_path)
            else:
                shutil.copyfile(wav_path, out_path)

    mp3_path = _write_mp3(wav_path)
    result = {
        "ok": True,
        "path": str(out_path),
        "wavPath": str(wav_path),
        "mp3Path": str(mp3_path) if mp3_path else "",
        "sampleRate": sample_rate,
        "playtimeMs": playtime_ms,
        "speakerSeed": int(args.speaker_seed),
        "speakerEmbedding": str(Path(args.speaker_embedding)) if args.speaker_embedding.strip() else "",
        "inferPrompt": infer_prompt,
        "refinePrompt": refine_prompt,
        "text": text,
    }
    out_path.with_suffix(".json").write_text(json.dumps(result, ensure_ascii=False), encoding="utf-8")
    print(json.dumps(result, ensure_ascii=True))


if __name__ == "__main__":
    main()
