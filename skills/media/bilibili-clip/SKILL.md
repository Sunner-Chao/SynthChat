---
name: bilibili-clip
description: Download Bilibili videos and optionally clip segments with the bundled Python script. Use when the user wants to fetch a B站 video by BV/av/link, save login cookies by QR code, choose 1080P or 4K, or export a clip by start/end/duration.
---

# Bilibili Clip

Use the bundled script instead of rewriting Bilibili download logic ad hoc.

## Resources

- Script: `scripts/bilibili_clip.py`

## Workflow

1. Confirm whether the user wants login only, a full download, or a clipped segment.
2. Check dependencies before execution:
   - `python --version`
   - `yt-dlp --version`
   - `ffmpeg -version`
   - `qrcode` is optional and only affects terminal QR rendering
3. For login requests, prefer opening the printed QR login URL in a browser tool when browser MCP/internal browser tools are available. If browser tools are unavailable, let the script open the system browser or show the terminal QR code.
4. Run the bundled script in CLI mode from this skill directory.

## Common Commands

```bash
python scripts/bilibili_clip.py --cli --login-only
python scripts/bilibili_clip.py --cli BV17d4y1u78H -q 1080P -o ./downloads
python scripts/bilibili_clip.py --cli BV17d4y1u78H --start 00:00:05 --duration 10 -o ./downloads
python scripts/bilibili_clip.py --cli https://www.bilibili.com/video/BV17d4y1u78H --login -q 4K -o ./downloads
```

## Notes

- 1080P and above require login.
- 4K may require Bilibili premium access.
- The script stores the default cookie file next to itself unless the user passes `--cookies`.
- During login, the script prints the QR login URL explicitly so it can be opened with `browser_navigate`, a browser session tool, or a normal browser.
- If the user gives an explicit output directory, pass it through `-o` instead of changing the script.
- Only patch the script when the user explicitly wants new behavior or a bug fix.
