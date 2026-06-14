"""SynthChat MCP Server - document understanding for common local files.

The server reads local files and returns extracted text/metadata for agents.

Configuration:
  - SYNTHCHAT_DOCUMENT_READER_MAX_CHARS: max returned characters (default: 20000)
"""

from __future__ import annotations

import csv
import base64
import html
import json
import os
import re
import unicodedata
import zipfile
from io import BytesIO
from pathlib import Path
from typing import Iterable
from xml.etree import ElementTree

from mcp.server.fastmcp import FastMCP

server = FastMCP("document-reader")

DEFAULT_MAX_CHARS = int(os.environ.get("SYNTHCHAT_DOCUMENT_READER_MAX_CHARS", "20000") or "20000")
PDF_OCR_ENABLED = os.environ.get("SYNTHCHAT_DOCUMENT_READER_PDF_OCR", "true").strip().lower() not in {
    "0",
    "false",
    "no",
    "off",
}
PDF_OCR_MAX_PAGES_RAW = int(os.environ.get("SYNTHCHAT_DOCUMENT_READER_PDF_OCR_MAX_PAGES", "0") or "0")
PDF_OCR_MAX_PAGES = None if PDF_OCR_MAX_PAGES_RAW <= 0 else max(1, min(PDF_OCR_MAX_PAGES_RAW, 500))
PDF_OCR_DPI = max(96, min(int(os.environ.get("SYNTHCHAT_DOCUMENT_READER_PDF_OCR_DPI", "144") or "144"), 240))
VISION_MODEL = (
    os.environ.get("SYNTHCHAT_VISION_MODEL")
    or os.environ.get("SYNTHCHAT_IMAGE_RECOGNITION_MODEL")
    or "llava:7b"
).strip()
VISION_FALLBACK_MODELS = [
    item.strip()
    for item in os.environ.get("SYNTHCHAT_VISION_OLLAMA_FALLBACK_MODELS", "moondream:1.8b,minicpm-v:latest").split(",")
    if item.strip()
]
OLLAMA_HOST = (
    os.environ.get("SYNTHCHAT_OLLAMA_HOST")
    or os.environ.get("SYNTHCHAT_IMAGE_RECOGNITION_BASE_URL")
    or "http://localhost:11434"
).strip()
TEXT_EXTENSIONS = {
    ".txt",
    ".md",
    ".markdown",
    ".csv",
    ".json",
    ".jsonl",
    ".log",
    ".xml",
    ".yaml",
    ".yml",
    ".html",
    ".htm",
    ".rtf",
}


def _limit(text: str, max_chars: int) -> str:
    if len(text) <= max_chars:
        return text
    return text[:max_chars] + "\n\n[内容已截断]"


def _clean_text(text: str) -> str:
    text = "".join(ch if ch in "\n\r\t" or not _is_control_char(ch) else " " for ch in text)
    text = re.sub(r"[ \t]{2,}", " ", text)
    return re.sub(r"\n{3,}", "\n\n", text).strip()


def _is_control_char(ch: str) -> bool:
    return unicodedata.category(ch).startswith("C")


def _looks_like_scanned_or_bad_pdf_text(text: str) -> bool:
    if not text.strip():
        return True
    total = max(1, len(text))
    control_count = sum(1 for ch in text if _is_control_char(ch) and ch not in "\n\r\t")
    replacement_count = text.count("\ufffd")
    visible = sum(1 for ch in text if ch.isprintable() and not ch.isspace())
    cjk_or_alnum = sum(1 for ch in text if "\u4e00" <= ch <= "\u9fff" or ch.isalnum())
    if control_count / total > 0.01 or replacement_count / total > 0.01:
        return True
    if visible < 80:
        return True
    if cjk_or_alnum / max(1, visible) < 0.35:
        return True
    return False


def _read_text(path: Path) -> str:
    data = path.read_bytes()
    for encoding in ("utf-8-sig", "utf-8", "gb18030", "utf-16", "latin-1"):
        try:
            return data.decode(encoding)
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", errors="replace")


def _html_to_text(raw: str) -> str:
    text = re.sub(r"(?is)<(script|style).*?>.*?</\1>", "\n", raw)
    text = re.sub(r"(?i)<br\s*/?>", "\n", text)
    text = re.sub(r"(?i)</p\s*>", "\n", text)
    text = re.sub(r"(?i)</div\s*>", "\n", text)
    text = re.sub(r"(?i)</tr\s*>", "\n", text)
    text = re.sub(r"(?i)</t[dh]\s*>", "\t", text)
    text = re.sub(r"(?s)<[^>]+>", " ", text)
    return _clean_text(html.unescape(text))


def _read_html_like_bytes(data: bytes) -> str:
    for encoding in ("utf-8-sig", "utf-8", "gb18030", "utf-16", "latin-1"):
        try:
            return _html_to_text(data.decode(encoding))
        except UnicodeDecodeError:
            continue
    return _html_to_text(data.decode("utf-8", errors="replace"))


def _read_csv(path: Path, max_rows: int) -> str:
    text = _read_text(path)
    rows = []
    dialect = csv.Sniffer().sniff(text[:4096]) if text.strip() else csv.excel
    for index, row in enumerate(csv.reader(text.splitlines(), dialect)):
        if index >= max_rows:
            rows.append("[行数已截断]")
            break
        rows.append(" | ".join(row))
    return "\n".join(rows)


def _xml_text_fragments(xml_bytes: bytes) -> Iterable[str]:
    root = ElementTree.fromstring(xml_bytes)
    for node in root.iter():
        tag = node.tag.rsplit("}", 1)[-1]
        if tag in {"t", "instrText"} and node.text:
            yield node.text
        elif tag in {"tab"}:
            yield "\t"
        elif tag in {"br", "cr", "p"}:
            yield "\n"


def _read_docx(path: Path) -> str:
    with zipfile.ZipFile(path) as archive:
        names = set(archive.namelist())
        parts = []
        for name in sorted(names):
            if name == "word/document.xml" or name.startswith("word/header") or name.startswith("word/footer"):
                parts.extend(_xml_text_fragments(archive.read(name)))
        return re.sub(r"\n{3,}", "\n\n", "".join(parts)).strip()


def _xlsx_shared_strings(archive: zipfile.ZipFile) -> list[str]:
    if "xl/sharedStrings.xml" not in archive.namelist():
        return []
    root = ElementTree.fromstring(archive.read("xl/sharedStrings.xml"))
    strings = []
    for si in root:
        strings.append("".join(node.text or "" for node in si.iter() if node.tag.rsplit("}", 1)[-1] == "t"))
    return strings


def _read_xlsx(path: Path, max_rows: int) -> str:
    with zipfile.ZipFile(path) as archive:
        shared = _xlsx_shared_strings(archive)
        sheet_names = [name for name in archive.namelist() if name.startswith("xl/worksheets/sheet") and name.endswith(".xml")]
        output = []
        for sheet_name in sorted(sheet_names):
            output.append(f"# {sheet_name}")
            root = ElementTree.fromstring(archive.read(sheet_name))
            row_count = 0
            for row in root.iter():
                if row.tag.rsplit("}", 1)[-1] != "row":
                    continue
                if row_count >= max_rows:
                    output.append("[行数已截断]")
                    break
                cells = []
                for cell in row:
                    if cell.tag.rsplit("}", 1)[-1] != "c":
                        continue
                    cell_type = cell.attrib.get("t", "")
                    value = ""
                    for child in cell:
                        if child.tag.rsplit("}", 1)[-1] == "v" and child.text:
                            value = child.text
                            break
                    if cell_type == "s" and value.isdigit():
                        index = int(value)
                        value = shared[index] if index < len(shared) else value
                    cells.append(value)
                if cells:
                    output.append(" | ".join(cells))
                    row_count += 1
        return "\n".join(output).strip()


def _slide_sort_key(name: str) -> int:
    match = re.search(r"slide(\d+)\.xml$", name)
    return int(match.group(1)) if match else 0


def _read_pptx(path: Path) -> str:
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        slide_names = sorted(
            [name for name in names if name.startswith("ppt/slides/slide") and name.endswith(".xml")],
            key=_slide_sort_key,
        )
        output = []
        for index, slide_name in enumerate(slide_names, start=1):
            text = "".join(_xml_text_fragments(archive.read(slide_name))).strip()
            text = re.sub(r"\n{3,}", "\n\n", text)
            if text:
                output.append(f"# Slide {index}\n{text}")
            else:
                output.append(f"# Slide {index}\n[无可提取文本]")
        note_names = sorted(
            [name for name in names if name.startswith("ppt/notesSlides/notesSlide") and name.endswith(".xml")]
        )
        notes = []
        for index, note_name in enumerate(note_names, start=1):
            text = "".join(_xml_text_fragments(archive.read(note_name))).strip()
            text = re.sub(r"\n{3,}", "\n\n", text)
            if text:
                notes.append(f"# Notes {index}\n{text}")
        if notes:
            output.append("\n\n## Speaker Notes\n" + "\n\n".join(notes))
        return "\n\n".join(output).strip()


def _candidate_vision_models() -> list[str]:
    models = [VISION_MODEL]
    for model in VISION_FALLBACK_MODELS:
        if model and model not in models:
            models.append(model)
    return models


def _pdf_pages_to_jpegs(path: Path, max_pages: int | None) -> list[bytes]:
    import fitz  # type: ignore
    from PIL import Image  # type: ignore

    images: list[bytes] = []
    zoom = PDF_OCR_DPI / 72
    matrix = fitz.Matrix(zoom, zoom)
    with fitz.open(str(path)) as doc:
        page_count = len(doc) if max_pages is None else min(len(doc), max_pages)
        for page_index in range(page_count):
            pix = doc[page_index].get_pixmap(matrix=matrix, alpha=False)
            image = Image.frombytes("RGB", [pix.width, pix.height], pix.samples)
            image.thumbnail((1600, 1600))
            buffer = BytesIO()
            image.save(buffer, format="JPEG", quality=86, optimize=True)
            images.append(buffer.getvalue())
    return images


def _ocr_pdf_with_vision(path: Path) -> str:
    if not PDF_OCR_ENABLED:
        return "扫描版 PDF 需要 OCR，但 SYNTHCHAT_DOCUMENT_READER_PDF_OCR 已关闭。"
    try:
        import ollama  # type: ignore
    except Exception as exc:
        return f"扫描版 PDF 需要 OCR，但缺少 ollama Python 库：{exc}"
    try:
        images = _pdf_pages_to_jpegs(path, PDF_OCR_MAX_PAGES)
    except Exception as exc:
        return f"扫描版 PDF 页面渲染失败：{exc}"
    if not images:
        return "扫描版 PDF 没有可渲染页面。"

    client = ollama.Client(host=OLLAMA_HOST)
    try:
        listed = client.list()
        available_names = [item.get("model", item.get("name", "")) for item in listed.get("models", [])]
    except Exception as exc:
        return f"扫描版 PDF 需要本地视觉模型 OCR，但无法连接 Ollama ({OLLAMA_HOST})：{exc}"
    candidates = _candidate_vision_models()
    available = [
        model
        for model in candidates
        if any(model in name or name.startswith(model.split(":")[0]) for name in available_names if name)
    ]
    if not available:
        return (
            "扫描版 PDF 需要本地视觉模型 OCR，但 Ollama 没有可用视觉模型："
            f"{', '.join(candidates)}。当前模型：{', '.join(available_names) or '无'}"
        )

    pages = []
    prompt = (
        "请对这页扫描版 PDF 做 OCR。优先逐行转写可见文字；如果文字不清晰，"
        "请说明版面、图表、印章、表格和可辨认内容。使用中文回答，不要编造看不清的文字。"
    )
    for index, image_bytes in enumerate(images, start=1):
        encoded = base64.b64encode(image_bytes).decode("ascii")
        last_error = None
        for model in available:
            try:
                response = client.generate(model=model, prompt=prompt, images=[encoded])
                text = (response.get("response", "") or "").strip()
                pages.append(f"# Page {index} OCR\n{text or 'OCR 未返回内容。'}")
                break
            except Exception as exc:
                last_error = exc
        else:
            pages.append(f"# Page {index} OCR\nOCR 失败：{last_error}")
    if PDF_OCR_MAX_PAGES is not None and len(images) == PDF_OCR_MAX_PAGES:
        pages.append(f"[OCR 仅处理前 {PDF_OCR_MAX_PAGES} 页，可通过 SYNTHCHAT_DOCUMENT_READER_PDF_OCR_MAX_PAGES=0 改为全部页]")
    return "\n\n".join(pages).strip()


def _ocr_pdf_with_rapidocr(path: Path) -> str:
    if not PDF_OCR_ENABLED:
        return "扫描版 PDF 需要 OCR，但 SYNTHCHAT_DOCUMENT_READER_PDF_OCR 已关闭。"
    try:
        from rapidocr_onnxruntime import RapidOCR  # type: ignore
    except Exception as exc:
        return f"扫描版 PDF 需要 OCR，但缺少 rapidocr_onnxruntime：{exc}"
    try:
        images = _pdf_pages_to_jpegs(path, PDF_OCR_MAX_PAGES)
    except Exception as exc:
        return f"扫描版 PDF 页面渲染失败：{exc}"
    if not images:
        return "扫描版 PDF 没有可渲染页面。"

    engine = RapidOCR()
    pages = []
    for index, image_bytes in enumerate(images, start=1):
        try:
            result, _elapsed = engine(image_bytes)
            lines = [item[1].strip() for item in (result or []) if len(item) >= 2 and str(item[1]).strip()]
            pages.append(f"# Page {index} OCR\n" + ("\n".join(lines) if lines else "OCR 未识别到文字。"))
        except Exception as exc:
            pages.append(f"# Page {index} OCR\nOCR 失败：{exc}")
    if PDF_OCR_MAX_PAGES is not None and len(images) == PDF_OCR_MAX_PAGES:
        pages.append(f"[OCR 仅处理前 {PDF_OCR_MAX_PAGES} 页，可通过 SYNTHCHAT_DOCUMENT_READER_PDF_OCR_MAX_PAGES=0 改为全部页]")
    return "\n\n".join(pages).strip()


def _ocr_pdf(path: Path) -> str:
    rapid = _ocr_pdf_with_rapidocr(path)
    if "缺少 rapidocr_onnxruntime" not in rapid and "OCR 未识别到文字" not in rapid and "OCR 失败" not in rapid:
        return rapid
    vision = _ocr_pdf_with_vision(path)
    return f"{rapid}\n\n[RapidOCR 不可用或结果不足，已尝试视觉模型]\n\n{vision}".strip()


def _read_pdf(path: Path) -> str:
    try:
        from pypdf import PdfReader  # type: ignore

        reader = PdfReader(str(path))
        pages = []
        for index, page in enumerate(reader.pages):
            pages.append(f"# Page {index + 1}\n{page.extract_text() or ''}")
        text = "\n\n".join(pages).strip()
        if _looks_like_scanned_or_bad_pdf_text(text):
            ocr = _ocr_pdf(path)
            return "[普通 PDF 文本抽取质量较差，已尝试扫描版 OCR]\n\n" + ocr
        return _clean_text(text)
    except Exception as exc:
        data = path.read_bytes()
        snippets = re.findall(rb"\(([^()]{4,})\)", data)
        text = "\n".join(bytes(item).decode("latin-1", errors="ignore") for item in snippets)
        if text.strip():
            if _looks_like_scanned_or_bad_pdf_text(text):
                ocr = _ocr_pdf(path)
                return "[普通 PDF 文本抽取质量较差，已尝试扫描版 OCR]\n\n" + ocr
            return _clean_text(text)
        return f"PDF 文本提取失败：{exc}。可安装 pypdf 以提升 PDF 解析能力。"


def _extract_printable(data: bytes) -> str:
    """Extract readable text from binary data (OLE streams, etc.)."""
    # Try UTF-16LE first (common in Word binary docs)
    try:
        text = data.decode("utf-16-le", errors="ignore")
        printable = "".join(c for c in text if c.isprintable() or c in "\n\r\t")
        if len(printable) > 50:
            return re.sub(r"\n{3,}", "\n\n", printable)
    except Exception:
        pass
    # Fallback: ASCII printable extraction
    chars = []
    for byte in data:
        if 32 <= byte < 127 or byte in (10, 13, 9):
            chars.append(chr(byte))
    text = "".join(chars)
    return re.sub(r"\n{3,}", "\n\n", text)


def _read_doc(path: Path) -> str:
    """Read legacy .doc (OLE2) files using olefile."""
    header = path.read_bytes()[:4096]
    stripped = header.lstrip().lower()
    if header.startswith(b"PK\x03\x04"):
        return _read_docx(path)
    if stripped.startswith(b"<!doctype html") or stripped.startswith(b"<html") or b"<html" in stripped[:512]:
        return _read_html_like_bytes(path.read_bytes())
    if not header.startswith(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"):
        text = _read_html_like_bytes(path.read_bytes())
        return text if text.strip() else _extract_printable(path.read_bytes())

    try:
        import olefile  # type: ignore
    except ImportError:
        return "旧版 .doc 文件需要 olefile 库。请运行: pip install olefile"

    try:
        ole = olefile.OleFileIO(str(path))
    except Exception as exc:
        return f"无法打开 .doc 文件：{exc}"

    text = ""
    if ole.exists("WordDocument"):
        word_stream = ole.openstream("WordDocument").read()
        # Try to extract via 1Table/0Table for structured text
        for table_name in ["1Table", "0Table"]:
            if ole.exists(table_name):
                table_stream = ole.openstream(table_name).read()
                combined = word_stream + table_stream
                text = _extract_printable(combined)
                if len(text.strip()) > 50:
                    ole.close()
                    return text
        # Fallback: extract from WordDocument stream alone
        text = _extract_printable(word_stream)
        if text.strip():
            ole.close()
            return text

    # Generic OLE fallback: extract printable text from all streams
    parts = []
    for stream_name in ole.listdir():
        name = "/".join(stream_name)
        try:
            data = ole.openstream(stream_name).read()
            stream_text = _extract_printable(data)
            if stream_text.strip():
                parts.append(f"# {name}\n{stream_text}")
        except Exception:
            pass
    ole.close()
    return "\n\n".join(parts).strip() or "无法从 .doc 文件中提取文本内容。"


@server.tool()
async def read_document(path: str, max_chars: int = DEFAULT_MAX_CHARS, max_rows: int = 200):
    """读取并理解常用本地文件，支持 txt/md/csv/json/pdf/doc/docx/ppt/pptx/xls/xlsx/xlsm。

    Args:
        path: 本地文件绝对路径
        max_chars: 最大返回字符数
        max_rows: 表格最大返回行数
    """
    file_path = Path(path).expanduser()
    if not file_path.is_file():
        return f"错误: 文件不存在: {path}"

    suffix = file_path.suffix.lower()
    max_chars = max(500, min(int(max_chars or DEFAULT_MAX_CHARS), 200000))
    max_rows = max(1, min(int(max_rows or 200), 5000))

    try:
        if suffix == ".pdf":
            content = _read_pdf(file_path)
        elif suffix == ".docx":
            content = _read_docx(file_path)
        elif suffix == ".pptx":
            content = _read_pptx(file_path)
        elif suffix in {".xlsx", ".xlsm"}:
            content = _read_xlsx(file_path, max_rows)
        elif suffix == ".csv":
            content = _read_csv(file_path, max_rows)
        elif suffix in TEXT_EXTENSIONS or not suffix:
            content = _read_text(file_path)
        elif suffix == ".xls":
            try:
                import xlrd  # type: ignore

                book = xlrd.open_workbook(str(file_path))
                rows = []
                for sheet in book.sheets():
                    rows.append(f"# {sheet.name}")
                    for row_index in range(min(sheet.nrows, max_rows)):
                        rows.append(" | ".join(str(value) for value in sheet.row_values(row_index)))
                    if sheet.nrows > max_rows:
                        rows.append("[行数已截断]")
                content = "\n".join(rows)
            except Exception as exc:
                content = f"旧版 .xls 读取失败：{exc}。可转换为 .xlsx，或安装 xlrd 以提升解析能力。"
        elif suffix == ".doc":
            content = _read_doc(file_path)
        elif suffix == ".ppt":
            content = "旧版 .ppt 二进制演示文稿暂不支持直接解析；请另存为 .pptx 或 PDF 后读取。"
        else:
            content = f"暂不支持的文件类型: {suffix or '(无扩展名)'}"
    except Exception as exc:
        content = f"读取文件失败: {exc}"

    payload = {
        "path": str(file_path),
        "fileName": file_path.name,
        "extension": suffix,
        "sizeBytes": file_path.stat().st_size,
        "text": _limit(content, max_chars),
    }
    return json.dumps(payload, ensure_ascii=False, indent=2)


def _check_optional_deps():
    """Log availability of optional dependencies at startup."""
    import sys

    deps = {
        "pypdf": "PDF 文本提取",
        "fitz": "扫描版 PDF 页面渲染",
        "PIL": "扫描版 PDF 图片处理",
        "rapidocr_onnxruntime": "扫描版 PDF 本地 OCR",
        "ollama": "扫描版 PDF 本地视觉 OCR",
        "olefile": "旧版 .doc 文件解析",
        "xlrd": "旧版 .xls 文件解析",
    }
    for pkg, desc in deps.items():
        try:
            __import__(pkg)
        except ImportError:
            print(f"[document_reader] 可选依赖 {pkg} 未安装（{desc}）。", file=sys.stderr)


_check_optional_deps()

if __name__ == "__main__":
    server.run(transport="stdio")
