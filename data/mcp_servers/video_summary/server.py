"""SynthChat MCP Server - video transcript and summary source extraction.

This is a lightweight BiliNote-style adapter:
1. Prefer platform subtitles.
2. Fall back to yt-dlp subtitle metadata when available.
3. Fall back to local STT audio transcription when subtitles are unavailable.
4. Return transcript + metadata for the Agent/LLM to summarize.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from dataclasses import asdict, dataclass
from html import unescape
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import parse_qs, urlencode, urlparse
from urllib.request import Request, urlopen

from mcp.server.fastmcp import FastMCP

server = FastMCP("video-summary")


@dataclass
class TranscriptSegment:
    start: float
    end: float
    text: str


def _timeout() -> float:
    raw = os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_TIMEOUT_SECONDS", "20").strip()
    try:
        value = float(raw)
    except ValueError:
        value = 20.0
    return max(3.0, min(value, 120.0))


def _download_timeout() -> float:
    raw = os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_DOWNLOAD_TIMEOUT_SECONDS", "600").strip()
    try:
        value = float(raw)
    except ValueError:
        value = 600.0
    return max(_timeout(), min(value, 3600.0))


def _yt_dlp_info_timeout() -> float:
    raw = os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_YTDLP_INFO_TIMEOUT_SECONDS", "120").strip()
    try:
        value = float(raw)
    except ValueError:
        value = 120.0
    return max(_timeout(), min(value, _download_timeout(), 3600.0))


def _user_agent() -> str:
    return os.environ.get(
        "SYNTHCHAT_VIDEO_SUMMARY_USER_AGENT",
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
        "Chrome/126.0 Safari/537.36 SynthChat/0.1.8",
    )


def _cookie_header() -> str:
    return os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_COOKIE", "").strip()


def _cookie_file() -> str:
    return os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_COOKIE_FILE", "").strip()


def _auth_headers(headers: dict[str, str] | None = None) -> dict[str, str]:
    output = {"User-Agent": _user_agent(), **(headers or {})}
    cookie = _cookie_header()
    if cookie:
        output["Cookie"] = cookie
    return output


def _json_get(url: str, headers: dict[str, str] | None = None) -> dict[str, Any]:
    request = Request(url, headers=_auth_headers(headers))
    with urlopen(request, timeout=_timeout()) as response:  # noqa: S310 - user supplied video URLs.
        return json.loads(response.read().decode("utf-8", errors="replace"))


def _text_get(url: str, headers: dict[str, str] | None = None) -> str:
    request = Request(url, headers=_auth_headers(headers))
    with urlopen(request, timeout=_timeout()) as response:  # noqa: S310 - user supplied subtitle URLs.
        return response.read().decode("utf-8", errors="replace")


def _platform_from_url(url: str, platform: str = "") -> str:
    if platform:
        return platform.lower()
    if _local_media_path(url):
        return "local_file"
    host = urlparse(url).netloc.lower()
    if "bilibili.com" in host or "b23.tv" in host:
        return "bilibili"
    if "youtube.com" in host or "youtu.be" in host:
        return "youtube"
    return "unknown"


def _extract_bvid(url: str) -> str | None:
    match = re.search(r"(BV[a-zA-Z0-9]+)", url)
    return match.group(1) if match else None


def _clean_text(value: str) -> str:
    return re.sub(r"\s+", " ", unescape(value or "")).strip()


def _segments_to_text(segments: list[TranscriptSegment], max_chars: int) -> str:
    lines: list[str] = []
    for segment in segments:
        minute = int(segment.start // 60)
        second = int(segment.start % 60)
        lines.append(f"[{minute:02d}:{second:02d}] {segment.text}")
    text = "\n".join(lines)
    if len(text) > max_chars:
        return text[:max_chars] + "\n\n[字幕内容已截断]"
    return text


def _parse_bilibili_subtitle_body(payload: dict[str, Any]) -> list[TranscriptSegment]:
    body = payload.get("body") or []
    output: list[TranscriptSegment] = []
    for item in body:
        try:
            start = float(item.get("from") or item.get("start") or 0)
            end = float(item.get("to") or item.get("end") or start)
        except (TypeError, ValueError):
            continue
        text = _clean_text(str(item.get("content") or item.get("text") or ""))
        if text:
            output.append(TranscriptSegment(start=start, end=end, text=text))
    return output


def _fetch_bilibili_transcript(url: str) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    bvid = _extract_bvid(url)
    if not bvid:
        raise ValueError("无法从 Bilibili URL 中解析 BV 号")
    view = _json_get(
        f"https://api.bilibili.com/x/web-interface/view?{urlencode({'bvid': bvid})}",
        {"Referer": "https://www.bilibili.com"},
    )
    data = view.get("data") or {}
    pages = data.get("pages") or []
    cid = pages[0].get("cid") if pages else data.get("cid")
    if not cid:
        raise ValueError("Bilibili view API 未返回 cid")
    player = _json_get(
        f"https://api.bilibili.com/x/player/wbi/v2?{urlencode({'bvid': bvid, 'cid': cid})}",
        {"Referer": url},
    )
    subtitles = (((player.get("data") or {}).get("subtitle") or {}).get("subtitles") or [])
    if not subtitles:
        raise ValueError("该 Bilibili 视频没有公开字幕；可配置 Cookie/转写服务作为后续兜底")
    chosen = (
        next((item for item in subtitles if "zh" in str(item.get("lan", "")).lower()), None)
        or subtitles[0]
    )
    subtitle_url = str(chosen.get("subtitle_url") or "").strip()
    if subtitle_url.startswith("//"):
        subtitle_url = "https:" + subtitle_url
    if not subtitle_url:
        raise ValueError("Bilibili 字幕条目缺少 subtitle_url")
    transcript_payload = _json_get(subtitle_url, {"Referer": url})
    meta = {
        "title": data.get("title") or "",
        "author": (data.get("owner") or {}).get("name") or "",
        "duration": data.get("duration") or 0,
        "platform": "bilibili",
        "source": "bilibili_player_api",
        "subtitleLanguage": chosen.get("lan_doc") or chosen.get("lan") or "",
    }
    return meta, _parse_bilibili_subtitle_body(transcript_payload)


def _youtube_video_id(url: str) -> str | None:
    parsed = urlparse(url)
    if parsed.netloc.endswith("youtu.be"):
        return parsed.path.strip("/") or None
    query = parse_qs(parsed.query)
    return (query.get("v") or [None])[0]


def _fetch_youtube_transcript_api(url: str, language: str) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    try:
        from youtube_transcript_api import YouTubeTranscriptApi  # type: ignore
    except Exception as exc:  # pragma: no cover - optional dependency
        raise RuntimeError(f"youtube-transcript-api 未安装: {exc}") from exc
    video_id = _youtube_video_id(url)
    if not video_id:
        raise ValueError("无法从 YouTube URL 解析 video id")
    candidates = [language, "zh-Hans", "zh-CN", "zh", "en"]
    transcript = YouTubeTranscriptApi.get_transcript(video_id, languages=candidates)
    segments = [
        TranscriptSegment(
            start=float(item.get("start") or 0),
            end=float(item.get("start") or 0) + float(item.get("duration") or 0),
            text=_clean_text(str(item.get("text") or "")),
        )
        for item in transcript
        if _clean_text(str(item.get("text") or ""))
    ]
    return {"platform": "youtube", "source": "youtube_transcript_api", "videoId": video_id}, segments


def _yt_dlp_command() -> str:
    return os.environ.get("SYNTHCHAT_YT_DLP_COMMAND", "yt-dlp")


def _ffmpeg_location_args() -> list[str]:
    path = os.environ.get("SYNTHCHAT_FFMPEG_BIN_PATH", "").strip()
    return ["--ffmpeg-location", path] if path else []


def _yt_dlp_auth_args() -> list[str]:
    args: list[str] = []
    cookie_file = _cookie_file()
    if cookie_file:
        path = Path(cookie_file).expanduser()
        if path.is_file():
            args.extend(["--cookies", str(path)])
    cookie = _cookie_header()
    if cookie:
        args.extend(["--add-header", f"Cookie:{cookie}"])
    return args


def _fetch_ytdlp_info(url: str) -> dict[str, Any]:
    command = [_yt_dlp_command(), "-J", "--skip-download", *_yt_dlp_auth_args(), url]
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        timeout=_yt_dlp_info_timeout(),
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip() or "yt-dlp failed")
    return json.loads(result.stdout)


def _env_bool(name: str, default: bool = True) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    return raw.strip().lower() not in {"0", "false", "no", "off", "disabled"}


def _models_dir() -> Path:
    configured = os.environ.get("SYNTHCHAT_MODELS_DIR", "").strip()
    if configured:
        return Path(configured).expanduser()

    candidates: list[Path] = []
    for base in [Path.cwd(), Path(__file__).resolve()]:
        candidates.extend(parent / "models" for parent in [base, *base.parents])
    user_profile = os.environ.get("USERPROFILE") or os.environ.get("HOME")
    if user_profile:
        candidates.append(Path(user_profile).expanduser() / "models")
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return Path.cwd() / "models"


def _audio_work_dir() -> Path:
    configured = os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_OUTPUT_DIR", "").strip()
    base = Path(configured).expanduser() if configured else Path(tempfile.gettempdir())
    path = base / "audio"
    path.mkdir(parents=True, exist_ok=True)
    return path


def _download_audio(url: str) -> Path:
    local_path = _local_media_path(url)
    if local_path:
        return _extract_audio_from_file(local_path)
    yt_dlp = _yt_dlp_command()
    if shutil.which(yt_dlp) is None and not Path(yt_dlp).exists():
        raise RuntimeError(f"yt-dlp 未找到: {yt_dlp}")
    digest = hashlib.sha256(url.encode("utf-8")).hexdigest()[:16]
    output = str(_audio_work_dir() / f"{digest}.%(ext)s")
    command = [
        yt_dlp,
        "--no-playlist",
        "--force-overwrites",
        *_yt_dlp_auth_args(),
        "-x",
        "--audio-format",
        "wav",
        "--audio-quality",
        "0",
        *_ffmpeg_location_args(),
        "-o",
        output,
        url,
    ]
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        timeout=_download_timeout(),
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip() or "yt-dlp audio download failed")
    candidates = sorted(
        _audio_work_dir().glob(f"{digest}.*"),
        key=lambda item: item.stat().st_mtime,
        reverse=True,
    )
    for item in candidates:
        if item.suffix.lower() in {".wav", ".m4a", ".mp3", ".webm", ".opus", ".aac", ".flac"}:
            return item
    raise RuntimeError("音频下载完成但未找到可转写的音频文件")


_MEDIA_EXTENSIONS = {
    "mp4",
    "mkv",
    "mov",
    "avi",
    "wmv",
    "flv",
    "webm",
    "m4v",
    "mp3",
    "wav",
    "m4a",
    "aac",
    "flac",
    "opus",
    "ogg",
}


def _direct_local_media_path(value: str) -> Path | None:
    text = (value or "").strip().strip('"').strip("'")
    if not text:
        return None
    if text.lower().startswith("file://"):
        parsed = urlparse(text)
        text = parsed.path
        if re.match(r"^/[a-zA-Z]:/", text):
            text = text[1:]
    path = Path(text).expanduser()
    if not path.is_file():
        return None
    if path.suffix.lower().lstrip(".") not in _MEDIA_EXTENSIONS:
        return None
    return path


def _split_env_paths(value: str) -> list[Path]:
    paths: list[Path] = []
    for chunk in re.split(r"[;\n]", value or ""):
        text = chunk.strip().strip('"').strip("'")
        if text:
            paths.append(Path(text).expanduser())
    return paths


def _local_media_search_roots() -> list[Path]:
    roots = _split_env_paths(os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_SEARCH_ROOTS", ""))
    for env_name in ("USERPROFILE", "HOME"):
        user_home = os.environ.get(env_name, "").strip()
        if user_home:
            base = Path(user_home).expanduser()
            roots.extend(base / name for name in ("Desktop", "Downloads", "Documents", "Videos"))
    roots.append(Path.cwd())

    seen: set[str] = set()
    output: list[Path] = []
    for root in roots:
        try:
            resolved = root.resolve()
        except OSError:
            resolved = root
        key = str(resolved).lower() if os.name == "nt" else str(resolved)
        if key not in seen and root.exists() and root.is_dir():
            seen.add(key)
            output.append(root)
    return output


def _resolve_abbreviated_media_path(value: str) -> Path | None:
    text = (value or "").strip().strip('"').strip("'")
    if "..." not in text:
        return None
    filename = Path(text.replace("\\", "/")).name
    if not filename or Path(filename).suffix.lower().lstrip(".") not in _MEDIA_EXTENSIONS:
        return None

    lowered_suffix = text.replace("/", "\\").lower().split("...", 1)[-1].lstrip("\\")
    try:
        max_matches = int(os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_PATH_DISCOVERY_MAX_MATCHES", "20") or "20")
    except ValueError:
        max_matches = 20
    suffix_matches: list[Path] = []
    filename_matches: list[Path] = []
    for root in _local_media_search_roots():
        try:
            for candidate in root.rglob(filename):
                direct = _direct_local_media_path(str(candidate))
                if not direct:
                    continue
                normalized = str(direct).replace("/", "\\").lower()
                if lowered_suffix and normalized.endswith(lowered_suffix):
                    suffix_matches.append(direct)
                else:
                    filename_matches.append(direct)
                if len(suffix_matches) + len(filename_matches) >= max(1, max_matches):
                    break
        except (OSError, RuntimeError):
            continue
        if suffix_matches or filename_matches:
            break
    matches = suffix_matches or filename_matches
    if not matches:
        return None
    return max(matches, key=lambda item: item.stat().st_mtime)


def _local_media_path(value: str) -> Path | None:
    return _direct_local_media_path(value) or _resolve_abbreviated_media_path(value)


def _ffmpeg_command() -> str:
    configured = os.environ.get("SYNTHCHAT_FFMPEG_COMMAND", "").strip()
    if configured:
        return configured
    bin_path = os.environ.get("SYNTHCHAT_FFMPEG_BIN_PATH", "").strip()
    if bin_path:
        candidate = Path(bin_path)
        if candidate.is_dir():
            executable = "ffmpeg.exe" if os.name == "nt" else "ffmpeg"
            return str(candidate / executable)
        return str(candidate)
    return "ffmpeg"


def _extract_audio_from_file(path: Path) -> Path:
    if path.suffix.lower() in {".wav", ".mp3", ".m4a", ".aac", ".flac", ".opus", ".ogg"}:
        return path
    ffmpeg = _ffmpeg_command()
    if shutil.which(ffmpeg) is None and not Path(ffmpeg).exists():
        raise RuntimeError(f"ffmpeg 未找到: {ffmpeg}")
    digest = hashlib.sha256(str(path.resolve()).encode("utf-8")).hexdigest()[:16]
    output = _audio_work_dir() / f"local-{digest}.wav"
    command = [
        ffmpeg,
        "-y",
        "-i",
        str(path),
        "-vn",
        "-acodec",
        "pcm_s16le",
        "-ar",
        "16000",
        "-ac",
        "1",
        str(output),
    ]
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        timeout=_download_timeout(),
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip() or "ffmpeg audio extraction failed")
    return output


def _faster_whisper_model_path() -> Path | str:
    configured = os.environ.get("SYNTHCHAT_FASTER_WHISPER_MODEL_DIR", "").strip()
    if configured:
        return Path(configured).expanduser()
    model_name = os.environ.get("SYNTHCHAT_FASTER_WHISPER_MODEL", "small").strip() or "small"
    root = _models_dir()
    for candidate in [
        root / "faster-whisper" / model_name,
        root / "faster-whisper",
        root / f"faster-whisper-{model_name}",
    ]:
        if (candidate / "model.bin").exists() or (candidate / "config.json").exists():
            return candidate
    return model_name


def _sensevoice_model_path() -> Path:
    configured = os.environ.get("SYNTHCHAT_SENSEVOICE_MODEL_DIR", "").strip()
    if configured:
        return Path(configured).expanduser()
    root = _models_dir()
    for candidate in [
        root / "sensevoice" / "SenseVoiceSmall",
        root / "sensevoice" / "models" / "iic" / "SenseVoiceSmall",
        root / "SenseVoiceSmall",
    ]:
        if (candidate / "model.pt").exists() or (candidate / "configuration.json").exists():
            return candidate
    return root / "sensevoice" / "SenseVoiceSmall"


def _transcribe_with_faster_whisper(audio_path: Path, language: str) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    try:
        from faster_whisper import WhisperModel  # type: ignore
    except Exception as exc:  # pragma: no cover - optional dependency
        raise RuntimeError(f"faster-whisper 未安装: {exc}") from exc

    model_path = _faster_whisper_model_path()
    if isinstance(model_path, Path) and not model_path.exists():
        raise RuntimeError(f"faster-whisper 模型目录不存在: {model_path}")
    model = WhisperModel(
        str(model_path),
        device=os.environ.get("SYNTHCHAT_FASTER_WHISPER_DEVICE", "cpu"),
        compute_type=os.environ.get("SYNTHCHAT_FASTER_WHISPER_COMPUTE_TYPE", "int8"),
        download_root=str(_models_dir() / "faster-whisper"),
        local_files_only=isinstance(model_path, Path),
    )
    lang = None if language.lower() in {"", "auto"} else language.split("-")[0]
    items, info = model.transcribe(
        str(audio_path),
        language=lang,
        vad_filter=_env_bool("SYNTHCHAT_FASTER_WHISPER_VAD_FILTER", True),
        beam_size=int(os.environ.get("SYNTHCHAT_FASTER_WHISPER_BEAM_SIZE", "5")),
    )
    segments = [
        TranscriptSegment(float(item.start or 0), float(item.end or item.start or 0), _clean_text(item.text or ""))
        for item in items
        if _clean_text(item.text or "")
    ]
    meta = {
        "source": "audio_transcription:faster_whisper",
        "transcriber": "faster_whisper",
        "transcriptionLanguage": getattr(info, "language", "") or language,
        "modelPath": str(model_path),
        "audioPath": str(audio_path),
    }
    return meta, segments


def _transcribe_with_sensevoice(audio_path: Path, language: str) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    try:
        from funasr import AutoModel  # type: ignore
    except Exception as exc:  # pragma: no cover - optional dependency
        raise RuntimeError(f"funasr/SenseVoice 依赖未安装: {exc}") from exc

    model_path = _sensevoice_model_path()
    if not model_path.exists():
        raise RuntimeError(f"SenseVoice 模型目录不存在: {model_path}")
    model = AutoModel(
        model=str(model_path),
        trust_remote_code=True,
        disable_update=True,
        device=os.environ.get("SYNTHCHAT_SENSEVOICE_DEVICE", "cpu"),
    )
    lang = "auto" if language.lower() in {"", "auto"} else language
    result = model.generate(
        input=str(audio_path),
        language=lang,
        use_itn=True,
        batch_size_s=int(os.environ.get("SYNTHCHAT_SENSEVOICE_BATCH_SIZE_SECONDS", "60")),
        merge_vad=True,
        merge_length_s=int(os.environ.get("SYNTHCHAT_SENSEVOICE_MERGE_LENGTH_SECONDS", "15")),
    )
    texts: list[str] = []
    for item in result if isinstance(result, list) else [result]:
        if isinstance(item, dict):
            text = item.get("text") or item.get("value") or ""
        else:
            text = str(item)
        text = _clean_text(str(text))
        if text:
            texts.append(text)
    transcript = "\n".join(texts).strip()
    segments = [TranscriptSegment(0.0, 0.0, transcript)] if transcript else []
    meta = {
        "source": "audio_transcription:sensevoice",
        "transcriber": "sensevoice",
        "transcriptionLanguage": lang,
        "modelPath": str(model_path),
        "audioPath": str(audio_path),
    }
    return meta, segments


def _transcribe_audio(audio_path: Path, language: str, transcriber: str) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    selected = (transcriber or os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_TRANSCRIBER", "auto")).strip().lower()
    engines = ["faster_whisper", "sensevoice"] if selected == "auto" else [selected]
    errors: list[str] = []
    for engine in engines:
        try:
            if engine in {"faster_whisper", "faster-whisper", "whisper"}:
                return _transcribe_with_faster_whisper(audio_path, language)
            if engine == "sensevoice":
                return _transcribe_with_sensevoice(audio_path, language)
            if engine in {"none", "disabled", "off"}:
                raise RuntimeError("音频转写已关闭")
            raise RuntimeError(f"未知转写引擎: {engine}")
        except Exception as exc:
            errors.append(f"{engine}: {exc}")
    raise RuntimeError("; ".join(errors))


def _fetch_audio_transcript(
    url: str,
    language: str,
    transcriber: str,
) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    audio_path = _download_audio(url)
    return _transcribe_audio(audio_path, language, transcriber)


def _subtitle_candidates(info: dict[str, Any], language: str) -> list[dict[str, Any]]:
    groups = []
    for key in ("subtitles", "automatic_captions"):
        value = info.get(key)
        if isinstance(value, dict):
            groups.append(value)
    preferred = [language, "zh-Hans", "zh-CN", "zh", "en"]
    output: list[dict[str, Any]] = []
    for group in groups:
        for lang in preferred:
            output.extend(group.get(lang) or [])
        for entries in group.values():
            if isinstance(entries, list):
                output.extend(entries)
    seen = set()
    deduped = []
    for item in output:
        url = item.get("url")
        if not url or url in seen:
            continue
        seen.add(url)
        deduped.append(item)
    return deduped


def _parse_vtt(text: str) -> list[TranscriptSegment]:
    segments: list[TranscriptSegment] = []
    current_time: tuple[float, float] | None = None
    current_lines: list[str] = []

    def flush() -> None:
        nonlocal current_time, current_lines
        if current_time and current_lines:
            value = _clean_text(" ".join(current_lines))
            value = re.sub(r"<[^>]+>", "", value)
            if value:
                segments.append(TranscriptSegment(current_time[0], current_time[1], value))
        current_time = None
        current_lines = []

    for raw in text.splitlines():
        line = raw.strip()
        if not line or line == "WEBVTT" or line.startswith(("Kind:", "Language:")):
            flush()
            continue
        if "-->" in line:
            flush()
            left, right = line.split("-->", 1)
            current_time = (_parse_timestamp(left), _parse_timestamp(right.split()[0]))
        elif current_time and not line.isdigit():
            current_lines.append(line)
    flush()
    return segments


def _parse_timestamp(value: str) -> float:
    parts = value.strip().replace(",", ".").split(":")
    try:
        if len(parts) == 3:
            return int(parts[0]) * 3600 + int(parts[1]) * 60 + float(parts[2])
        if len(parts) == 2:
            return int(parts[0]) * 60 + float(parts[1])
        return float(parts[0])
    except (ValueError, IndexError):
        return 0.0


def _fetch_ytdlp_transcript(url: str, language: str) -> tuple[dict[str, Any], list[TranscriptSegment]]:
    info = _fetch_ytdlp_info(url)
    for item in _subtitle_candidates(info, language):
        sub_url = str(item.get("url") or "")
        ext = str(item.get("ext") or "").lower()
        if not sub_url:
            continue
        text = _text_get(sub_url)
        if ext in {"json3", "srv1", "srv2", "srv3"} or text.lstrip().startswith("{"):
            try:
                payload = json.loads(text)
                events = payload.get("events") or []
                segments = []
                for event in events:
                    segs = event.get("segs") or []
                    content = _clean_text("".join(str(seg.get("utf8") or "") for seg in segs))
                    if content:
                        start = float(event.get("tStartMs") or 0) / 1000
                        duration = float(event.get("dDurationMs") or 0) / 1000
                        segments.append(TranscriptSegment(start, start + duration, content))
                if segments:
                    return _ytdlp_meta(info, "yt_dlp_json_caption"), segments
            except Exception:
                pass
        segments = _parse_vtt(text)
        if segments:
            return _ytdlp_meta(info, "yt_dlp_caption"), segments
    raise ValueError("yt-dlp 没有发现可用字幕；需要配置音频转写服务")


def _ytdlp_meta(info: dict[str, Any], source: str) -> dict[str, Any]:
    return {
        "title": info.get("title") or "",
        "author": info.get("uploader") or info.get("channel") or "",
        "duration": info.get("duration") or 0,
        "platform": info.get("extractor_key") or "video",
        "source": source,
        "webpageUrl": info.get("webpage_url") or "",
    }


def _write_artifact(payload: dict[str, Any]) -> str:
    out_dir = Path(os.environ.get("SYNTHCHAT_VIDEO_SUMMARY_OUTPUT_DIR", "") or tempfile.gettempdir())
    out_dir.mkdir(parents=True, exist_ok=True)
    digest = hashlib.sha256(str(payload.get("url", "video")).encode("utf-8")).hexdigest()[:16]
    path = out_dir / f"synthchat-video-summary-{digest}.json"
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")
    return str(path)


@server.tool()
async def summarize_video(
    url: str = "",
    video_url: str = "",
    video_path: str = "",
    platform: str = "",
    language: str = "zh-CN",
    max_chars: int = 30000,
    transcriber: str = "auto",
    enable_transcribe: bool = True,
    allow_remote_transcribe: bool = False,
):
    """Extract a video transcript for Agent-side AI summary.

    Args:
        url: Video URL from Bilibili, YouTube, or a yt-dlp supported site.
        video_path: Local video/audio file path.
        platform: Optional platform hint, such as bilibili or youtube.
        language: Preferred subtitle language.
        max_chars: Maximum transcript characters returned to the Agent.
        transcriber: Optional STT engine, auto/faster_whisper/sensevoice/none.
        enable_transcribe: Whether to download audio and transcribe when subtitles fail.
    """
    clean_url = (video_path or url or video_url or "").strip()
    if not clean_url:
        return json.dumps({"ok": False, "error": "url or video_path is required"}, ensure_ascii=False)

    detected = _platform_from_url(clean_url, platform)
    is_local_source = detected == "local_file"
    errors: list[str] = []
    meta: dict[str, Any] = {"platform": detected}
    segments: list[TranscriptSegment] = []

    if not is_local_source:
        for fetcher in (
            [_fetch_bilibili_transcript] if detected == "bilibili" else []
        ) + (
            [_fetch_youtube_transcript_api] if detected == "youtube" else []
        ) + [_fetch_ytdlp_transcript]:
            try:
                if fetcher is _fetch_youtube_transcript_api or fetcher is _fetch_ytdlp_transcript:
                    meta, segments = fetcher(clean_url, language)  # type: ignore[misc]
                else:
                    meta, segments = fetcher(clean_url)  # type: ignore[misc]
                if segments:
                    break
            except (
                HTTPError,
                URLError,
                TimeoutError,
                RuntimeError,
                ValueError,
                OSError,
                subprocess.SubprocessError,
                json.JSONDecodeError,
            ) as exc:
                errors.append(f"{getattr(fetcher, '__name__', 'fetcher')}: {exc}")

    should_transcribe = (
        enable_transcribe
        and _env_bool("SYNTHCHAT_VIDEO_SUMMARY_ENABLE_TRANSCRIBE", True)
        and (is_local_source or allow_remote_transcribe)
    )
    if not segments and should_transcribe:
        try:
            transcript_meta, segments = _fetch_audio_transcript(clean_url, language, transcriber)
            meta.update(transcript_meta)
        except (TimeoutError, RuntimeError, ValueError, OSError, subprocess.SubprocessError) as exc:
            errors.append(f"audio_transcription: {exc}")
    elif not segments and enable_transcribe and not is_local_source:
        errors.append(
            "audio_transcription: 远程视频音频下载/转写默认跳过；如确需转写远程视频，请设置 allow_remote_transcribe=true"
        )

    if not segments:
        return json.dumps(
            {
                "ok": False,
                "url": clean_url,
                "platform": detected,
                "error": "未能获取字幕或音频转写文本。请检查 yt-dlp/ffmpeg/STT 依赖与模型目录配置。",
                "attempts": errors,
                "modelsDir": str(_models_dir()),
            },
            ensure_ascii=False,
            indent=2,
        )

    max_chars = max(1000, min(int(max_chars or 30000), 200000))
    payload = {
        "ok": True,
        "url": clean_url,
        **meta,
        "segmentCount": len(segments),
        "transcript": _segments_to_text(segments, max_chars),
        "segments": [asdict(item) for item in segments[:500]],
        "summaryInstruction": "请基于 transcript 生成结构化视频笔记：主题、核心观点、时间轴、行动清单和关键原文。",
        "warnings": errors,
    }
    payload["artifactPath"] = _write_artifact(payload)
    return json.dumps(payload, ensure_ascii=False, indent=2)


if __name__ == "__main__":
    server.run(transport="stdio")
