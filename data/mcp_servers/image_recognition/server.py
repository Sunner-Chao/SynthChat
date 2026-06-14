"""SynthChat MCP Server - Image recognition via Ollama local vision model.

为不支持多模态的模型（如 DeepSeek）提供图像识别能力。
接收 base64 编码的图片数据，调用本地 Ollama 视觉模型进行识别。

注意：此 MCP Server 需配合 capability_adapter 使用，由适配器
通过 $image_data(image_id) 注入 base64 图片数据。

配置项:
  - SYNTHCHAT_VISION_MODEL: Ollama 视觉模型名称 (默认: llava:7b)
  - SYNTHCHAT_IMAGE_RECOGNITION_MODEL: 同上，兼容 SynthChat 配置
  - SYNTHCHAT_OLLAMA_HOST: Ollama 服务地址 (默认: http://localhost:11434)
  - SYNTHCHAT_IMAGE_RECOGNITION_BASE_URL: 同上，兼容 SynthChat 配置
"""

from __future__ import annotations

import base64
import os
import sys
from io import BytesIO
from pathlib import Path

import ollama
from mcp.server.fastmcp import FastMCP

try:
    from PIL import Image
except Exception:  # pragma: no cover - optional runtime dependency
    Image = None

VISION_MODEL = (
    os.environ.get("SYNTHCHAT_VISION_MODEL")
    or os.environ.get("SYNTHCHAT_IMAGE_RECOGNITION_MODEL")
    or "llava:7b"
).strip()
FALLBACK_MODELS = [
    item.strip()
    for item in os.environ.get(
        "SYNTHCHAT_VISION_OLLAMA_FALLBACK_MODELS",
        "moondream:1.8b,minicpm-v:latest",
    ).split(",")
    if item.strip()
]
OLLAMA_HOST = (
    os.environ.get("SYNTHCHAT_OLLAMA_HOST")
    or os.environ.get("SYNTHCHAT_IMAGE_RECOGNITION_BASE_URL")
    or "http://localhost:11434"
).strip()

DATA_DIR = Path(
    sys.executable if getattr(sys, "frozen", False) else __file__
).resolve().parent.parent.parent

server = FastMCP("image-recognition")

# Configure Ollama client
ollama_client = ollama.Client(host=OLLAMA_HOST)


def _detect_mime(b64: str) -> str:
    """Detect image MIME type from base64 magic bytes."""
    try:
        header = base64.b64decode(b64[:32])
        if header[:8] == b"\x89PNG\r\n\x1a\n":
            return "image/png"
        if header[:2] == b"\xff\xd8":
            return "image/jpeg"
        if header[:4] == b"GIF8":
            return "image/gif"
        if header[:4] == b"RIFF" and header[8:12] == b"WEBP":
            return "image/webp"
    except Exception:
        pass
    return "image/jpeg"


SYSTEM_PROMPT = "你是一个图像识别助手。准确、详细地描述用户提供的图片内容。"


def _prepare_image_data(image_data: str) -> str:
    """Downscale and normalize image payload before sending to local vision models."""
    if Image is None:
        return image_data
    try:
        raw = base64.b64decode(image_data)
        max_edge = max(256, min(4096, int(os.environ.get("SYNTHCHAT_VISION_IMAGE_MAX_EDGE", "896"))))
        quality = max(50, min(95, int(os.environ.get("SYNTHCHAT_VISION_JPEG_QUALITY", "86"))))
        image = Image.open(BytesIO(raw)).convert("RGB")
        image.thumbnail((max_edge, max_edge), Image.Resampling.LANCZOS)
        output = BytesIO()
        image.save(output, format="JPEG", quality=quality, optimize=True)
        return base64.b64encode(output.getvalue()).decode("ascii")
    except Exception:
        return image_data


def _candidate_models() -> list[str]:
    models = [VISION_MODEL]
    for model in FALLBACK_MODELS:
        if model not in models:
            models.append(model)
    return models


@server.tool()
async def recognize_image(
    image_data: str = "",
    image_path: str = "",
    image_id: str = "",
    query: str = "请详细描述这张图片的内容",
):
    """识别并描述图片内容。接收 base64 图片数据或本地图片路径，返回文字描述。

    Args:
        image_data: base64 编码的图片数据（由 capability_adapter 注入）
        image_path: 本地图片文件路径
        image_id: 图片 ID（仅供参考）
        query: 关于图片的具体问题
    """
    if not image_data and image_path:
        path = Path(image_path).expanduser()
        if not path.is_file():
            return f"错误: 图片文件不存在: {image_path}"
        image_data = base64.b64encode(path.read_bytes()).decode("ascii")

    if not image_data:
        return "错误: 未收到图片数据或图片路径。请传入 image_path，或确保 capability_adapter 配置了 $image_data 注入。"

    image_data = _prepare_image_data(image_data)

    # Check if at least one vision model is available
    try:
        models = ollama_client.list()
        model_list = models.get("models", [])
        model_names = [m.get("model", m.get("name", "")) for m in model_list]
        candidates = _candidate_models()
        available = [
            model for model in candidates
            if any(model in name or name.startswith(model.split(":")[0]) for name in model_names if name)
        ]
        if not available:
            return (
                f"错误: 本地 Ollama 未找到可用视觉模型 '{', '.join(candidates)}'。\n"
                f"可用模型: {', '.join(model_names) or '无'}\n"
                f"请运行: ollama pull {candidates[0]}"
            )
    except Exception as exc:
        return f"错误: 无法连接 Ollama 服务 ({OLLAMA_HOST}): {exc}"

    _detect_mime(image_data)

    last_error = None
    for model in available:
        try:
            response = ollama_client.generate(
                model=model,
                system=SYSTEM_PROMPT,
                prompt=query,
                images=[image_data],  # Ollama accepts raw base64
            )
            text = response.get("response", "") or "图像识别未返回结果。"
            if model != VISION_MODEL:
                text += f"\n\n[识图服务提示：主模型 {VISION_MODEL} 失败后已自动降级到 {model}。]"
            return text
        except Exception as exc:
            last_error = exc
            message = str(exc)
            if "model runner has unexpectedly stopped" not in message and "500" not in message:
                return f"错误: 图像识别出错: {exc}"
    return f"错误: 图像识别出错: {last_error}"


if __name__ == "__main__":
    server.run(transport="stdio")
