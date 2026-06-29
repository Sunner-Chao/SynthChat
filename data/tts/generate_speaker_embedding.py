r"""Generate a ChatTTS speaker embedding from a seed and save as .pt file.

Usage:
    python generate_speaker_embedding.py --seed 20240 --out speaker_20240.pt
    python generate_speaker_embedding.py --seed 20240 --model-dir .\models\ChatTTS
"""
import argparse
import json
import sys
from pathlib import Path

DEFAULT_MODEL_DIR = Path("models") / "ChatTTS"


def main():
    parser = argparse.ArgumentParser(description="Generate ChatTTS speaker embedding")
    parser.add_argument("--seed", type=int, required=True, help="Speaker seed number")
    parser.add_argument("--out", default="", help="Output .pt file path (default: speaker_<seed>.pt in model-dir/speaker/)")
    parser.add_argument("--model-dir", default=str(DEFAULT_MODEL_DIR), help="ChatTTS model directory")
    args = parser.parse_args()

    model_dir = Path(args.model_dir)
    if not model_dir.exists():
        print(json.dumps({"ok": False, "error": f"Model directory not found: {model_dir}"}))
        sys.exit(1)

    # Determine output path
    if args.out:
        out_path = Path(args.out)
    else:
        speaker_dir = model_dir / "speaker"
        speaker_dir.mkdir(parents=True, exist_ok=True)
        out_path = speaker_dir / f"speaker_{args.seed}.pt"

    out_path.parent.mkdir(parents=True, exist_ok=True)

    try:
        import torch
        import ChatTTS
    except Exception as exc:
        print(json.dumps({"ok": False, "error": f"Import failed: {exc}"}))
        sys.exit(1)

    try:
        # Load ChatTTS model
        chat = ChatTTS.Chat()
        loaded = chat.load(source="custom", custom_path=str(model_dir), compile=False)
        if not loaded:
            print(json.dumps({"ok": False, "error": "ChatTTS model load returned false"}))
            sys.exit(1)

        # Generate speaker embedding with the given seed
        torch.manual_seed(args.seed)
        speaker_embedding = chat.sample_random_speaker()

        # Save as .pt file
        torch.save(speaker_embedding, str(out_path))

        result = {
            "ok": True,
            "seed": args.seed,
            "path": str(out_path),
            "shape": list(speaker_embedding.shape) if hasattr(speaker_embedding, "shape") else None,
        }
        print(json.dumps(result, ensure_ascii=False))

    except Exception as exc:
        print(json.dumps({"ok": False, "error": str(exc)}))
        sys.exit(1)


if __name__ == "__main__":
    main()
