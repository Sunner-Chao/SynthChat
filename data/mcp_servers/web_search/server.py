"""SynthChat MCP Server - Web Search via SearXNG.

This server intentionally avoids paid gateway APIs. It uses a local or
self-hosted SearXNG instance and calls its JSON search endpoint.

Configuration:
  SYNTHCHAT_SEARXNG_URLS, separated by semicolon or comma, or
  SYNTHCHAT_SEARXNG_URL / SEARXNG_URL for a single endpoint.

If no endpoint is configured, common local Docker mappings are tried.
"""

from __future__ import annotations

import json
import os
import re
import socket
from html import unescape
from html.parser import HTMLParser
from typing import Any
from urllib.parse import urlencode
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

from mcp.server.fastmcp import FastMCP

server = FastMCP("web-search")


def _request_timeout() -> float:
    raw = os.environ.get("SYNTHCHAT_WEB_SEARCH_TIMEOUT_SECONDS", "3").strip()
    try:
        value = float(raw)
    except ValueError:
        value = 3.0
    return max(0.5, min(value, 10.0))


def _candidate_base_urls() -> list[str]:
    configured = (
        os.environ.get("SYNTHCHAT_SEARXNG_URLS", "")
        or os.environ.get("SYNTHCHAT_SEARXNG_URL", "")
        or os.environ.get("SEARXNG_URL", "")
    )
    urls: list[str] = []
    for raw in configured.replace(",", ";").split(";"):
        value = raw.strip().rstrip("/")
        if value:
            urls.append(value)
    if not urls:
        urls.extend(
            [
                "http://127.0.0.1:8080",
                "http://localhost:8080",
                "http://127.0.0.1:8888",
                "http://localhost:8888",
                "http://127.0.0.1:8081",
                "http://localhost:8081",
                "http://127.0.0.1:18080",
                "http://localhost:18080",
            ]
        )
    return urls


def _fetch_json(url: str, timeout: float | None = None) -> dict[str, Any]:
    request = Request(
        url,
        headers={
            "Accept": "application/json",
            "User-Agent": "SynthChat/0.1.8 web_search MCP",
        },
    )
    with urlopen(request, timeout=timeout or _request_timeout()) as response:  # noqa: S310 - user-configured URL.
        return json.loads(response.read().decode("utf-8", errors="replace"))


def _fetch_text(url: str, timeout: float | None = None) -> str:
    request = Request(
        url,
        headers={
            "Accept": "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            "User-Agent": (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
                "AppleWebKit/537.36 (KHTML, like Gecko) "
                "Chrome/126.0 Safari/537.36 SynthChat/0.1.8"
            ),
        },
    )
    with urlopen(request, timeout=timeout or _request_timeout()) as response:  # noqa: S310 - user-configured URL.
        return response.read().decode("utf-8", errors="replace")


def _is_timeout_error(exc: BaseException) -> bool:
    if isinstance(exc, TimeoutError | socket.timeout):
        return True
    reason = getattr(exc, "reason", None)
    if isinstance(reason, TimeoutError | socket.timeout):
        return True
    text = str(exc).lower()
    return "timed out" in text or "timeout" in text


class _SearxngHtmlParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.results: list[dict[str, str]] = []
        self._in_article = False
        self._in_title = False
        self._in_content = False
        self._current: dict[str, str] = {}
        self._text: list[str] = []
        self._depth = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attrs_dict = {key: value or "" for key, value in attrs}
        class_name = attrs_dict.get("class", "")
        if tag == "article" and "result" in class_name:
            self._in_article = True
            self._current = {}
            self._depth = 1
            return
        if not self._in_article:
            return
        self._depth += 1
        if tag == "h3":
            self._in_title = True
            self._text = []
        elif tag == "p" and "content" in class_name:
            self._in_content = True
            self._text = []
        elif tag == "a" and self._in_title and not self._current.get("url"):
            href = attrs_dict.get("href", "").strip()
            if href:
                self._current["url"] = href

    def handle_endtag(self, tag: str) -> None:
        if not self._in_article:
            return
        if tag == "h3" and self._in_title:
            self._current["title"] = _clean_html_text("".join(self._text))
            self._in_title = False
            self._text = []
        elif tag == "p" and self._in_content:
            self._current["content"] = _clean_html_text("".join(self._text))
            self._in_content = False
            self._text = []
        self._depth -= 1
        if tag == "article" or self._depth <= 0:
            if self._current.get("title") or self._current.get("url"):
                self.results.append(self._current)
            self._in_article = False
            self._current = {}
            self._text = []
            self._depth = 0

    def handle_data(self, data: str) -> None:
        if self._in_article and (self._in_title or self._in_content):
            self._text.append(data)


def _clean_html_text(value: str) -> str:
    return re.sub(r"\s+", " ", unescape(value)).strip()


def _parse_html_results(html: str, limit: int) -> str:
    parser = _SearxngHtmlParser()
    parser.feed(html)
    if not parser.results:
        raise ValueError("response did not look like a SearXNG results page")
    lines: list[str] = []
    for index, item in enumerate(parser.results[:limit], start=1):
        title = item.get("title") or "无标题"
        url = item.get("url") or ""
        content = item.get("content") or ""
        lines.append(f"{index}. {title}")
        if url:
            lines.append(f"   URL: {url}")
        if content:
            lines.append(f"   摘要: {content}")
    return "\n".join(lines)


def _format_results(payload: dict[str, Any], limit: int) -> str:
    results = payload.get("results") or []
    if not results:
        return "未搜索到结果。"

    lines: list[str] = []
    for index, item in enumerate(results[:limit], start=1):
        title = str(item.get("title") or "无标题").strip()
        url = str(item.get("url") or "").strip()
        content = str(item.get("content") or item.get("snippet") or "").strip()
        engine = str(item.get("engine") or "").strip()
        lines.append(f"{index}. {title}")
        if url:
            lines.append(f"   URL: {url}")
        if engine:
            lines.append(f"   来源: {engine}")
        if content:
            lines.append(f"   摘要: {content}")
    return "\n".join(lines)


@server.tool()
async def web_search(query: str, count: int = 5, language: str = "zh-CN"):
    """Search the web through a local/self-hosted SearXNG instance.

    Args:
        query: Search keywords or question.
        count: Maximum number of results to return.
        language: Search language, such as zh-CN or en-US.
    """
    limit = max(1, min(int(count or 5), 10))
    json_params = urlencode(
        {
            "q": query,
            "format": "json",
            "language": language or "zh-CN",
            "safesearch": "0",
        }
    )
    html_params = urlencode(
        {
            "q": query,
            "language": language or "zh-CN",
            "safesearch": "0",
        }
    )
    errors: list[str] = []
    base_urls = _candidate_base_urls()
    for base_url in base_urls:
        try:
            payload = _fetch_json(f"{base_url}/search?{json_params}")
            return _format_results(payload, limit)
        except Exception as exc:
            json_error = exc
            if _is_timeout_error(exc):
                errors.append(f"{base_url}: json timeout after {_request_timeout():.1f}s")
                continue
        try:
            html = _fetch_text(f"{base_url}/search?{html_params}")
            return _parse_html_results(html, limit)
        except (HTTPError, URLError, TimeoutError, socket.timeout) as exc:
            if _is_timeout_error(exc):
                errors.append(f"{base_url}: json={json_error}; html timeout after {_request_timeout():.1f}s")
            else:
                errors.append(f"{base_url}: json={json_error}; html={exc}")
        except Exception as exc:
            errors.append(f"{base_url}: json={json_error}; html={exc}")
    return (
        "错误: SearXNG 搜索失败。已尝试配置地址或本地 Docker 常见端口。"
        "请确认 SearXNG 已启动，并在 web_search MCP 配置中设置 "
        "env.SYNTHCHAT_SEARXNG_URLS / SYNTHCHAT_SEARXNG_URL / SEARXNG_URL。\n"
        + "\n".join(errors)
    )


if __name__ == "__main__":
    server.run(transport="stdio")
