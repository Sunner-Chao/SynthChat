#!/usr/bin/env python3
"""
bilibili_clip.py - B站视频下载+裁剪工具 (支持 1080P / 4K)

从B站下载高清视频，支持裁剪片段。

用法:
    python bilibili_clip.py                  # 交互式引导
    python bilibili_clip.py BV17d4y1u78H     # 交互式，预填URL
    python bilibili_clip.py --cli --login BV17d4y1u78H     # 扫码登录后下载
    python bilibili_clip.py --cli --login-only              # 仅扫码登录
    python bilibili_clip.py --cli BV17d4y1u78H --start 5 --duration 10

依赖:
    - yt-dlp (pip install yt-dlp)
    - ffmpeg (系统PATH中可用)
    - qrcode (可选, pip install qrcode, 用于终端显示二维码)

注意:
    - 1080P 及以上画质需要B站登录
    - 4K 画质需要B站大会员
    - 首次使用建议先运行 --login-only 扫码登录保存Cookie
"""

import argparse
import http.cookiejar
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.parse
from pathlib import Path


# ── 常量 ────────────────────────────────────────────────────

QUALITY_MAP = {
    "1080P": {"height": 1080, "desc": "1080P 高清", "crf": 20, "desc_note": "需登录"},
    "4K":    {"height": 2160, "desc": "4K 超高清",   "crf": 18, "desc_note": "需大会员"},
}

# yt-dlp 画质格式选择字符串
QUALITY_FORMAT = {
    "1080P": "bv*[height<=1080]+ba/b[height<=1080]/bv+ba",
    "4K":    "bv*[height<=2160]+ba/b[height<=2160]/bv*+ba/bv+ba",
}


# ── 工具检测 ──────────────────────────────────────────────

def find_executable(name: str) -> str:
    path = shutil.which(name)
    if not path:
        print(f"[ERROR] {name} 未找到，请先安装:")
        if name == "yt-dlp":
            print("  pip install yt-dlp")
        elif name == "ffmpeg":
            print("  https://ffmpeg.org/download.html")
            print("  或: winget install ffmpeg")
        sys.exit(1)
    return path


def check_dependencies():
    yt_dlp = find_executable("yt-dlp")
    ffmpeg = find_executable("ffmpeg")
    return yt_dlp, ffmpeg


# ── 时间解析 ──────────────────────────────────────────────

def parse_time(value: str) -> float:
    """解析时间参数，支持秒数或 HH:MM:SS 格式"""
    if ":" in value:
        parts = value.split(":")
        if len(parts) == 2:
            return float(parts[0]) * 60 + float(parts[1])
        elif len(parts) == 3:
            return float(parts[0]) * 3600 + float(parts[1]) * 60 + float(parts[2])
        else:
            raise ValueError(f"无效时间格式: {value}")
    return float(value)


def format_time_str(seconds: float) -> str:
    """将秒数格式化为 HH:MM:SS.mmm"""
    h = int(seconds // 3600)
    m = int((seconds % 3600) // 60)
    s = seconds % 60
    if h > 0:
        return f"{h:02d}:{m:02d}:{s:05.2f}"
    return f"{m:02d}:{s:05.2f}"


# ── URL 规范化 ────────────────────────────────────────────

def normalize_url(url: str) -> str:
    """将 BV 号或链接规范化为完整 URL"""
    url = url.strip()
    if url.startswith("http"):
        return url
    if re.match(r"^BV[a-zA-Z0-9]+$", url):
        return f"https://www.bilibili.com/video/{url}"
    if re.match(r"^av\d+$", url, re.IGNORECASE):
        return f"https://www.bilibili.com/video/{url}"
    return url


# ── B站二维码登录 ─────────────────────────────────────────

BILI_QRCODE_GENERATE_URL = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate"
BILI_QRCODE_POLL_URL = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll"
COOKIES_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "bilibili_cookies.txt")


def _generate_qrcode(url: str) -> bool:
    """在终端生成二维码 ASCII 显示"""
    try:
        import qrcode
        qr = qrcode.QRCode(border=1)
        qr.add_data(url)
        qr.make(fit=True)
        qr.print_ascii(invert=True)
        return True
    except ImportError:
        return False


def qr_login() -> str | None:
    """通过B站二维码登录获取Cookie，返回cookies文件路径"""
    print()
    print("  ─── B站二维码登录 ───")
    print()

    # 1. 获取二维码
    print("  正在获取二维码...")
    req = urllib.request.Request(
        BILI_QRCODE_GENERATE_URL,
        headers={"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"}
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode())
    except Exception as e:
        print(f"  [ERROR] 获取二维码失败: {e}")
        return None

    if data.get("code") != 0:
        print(f"  [ERROR] 获取二维码失败: {data.get('message', '未知错误')}")
        return None

    qrcode_url = data["data"]["url"]
    qrcode_key = data["data"]["qrcode_key"]

    # 2. 显示二维码
    print()
    terminal_ok = _generate_qrcode(qrcode_url)
    print("  二维码链接:")
    print(f"  {qrcode_url}")
    if terminal_ok:
        # 终端二维码已显示
        print()
        print("  请使用B站APP扫描上方二维码，或在浏览器中打开上面的链接登录")
    else:
        # qrcode 库未安装，尝试打开浏览器
        print()
        try:
            # 将二维码URL转为扫码页面
            import webbrowser
            webbrowser.open(qrcode_url)
            print("  已在浏览器中打开二维码，请扫码登录")
        except Exception:
            print("  请手动复制上方链接到浏览器打开并扫码")

    print()
    print("  等待扫码中... (超时180秒)")

    # 3. 轮询扫码状态
    start_time = time.time()
    last_status = None

    while time.time() - start_time < 180:
        poll_params = urllib.parse.urlencode({"qrcode_key": qrcode_key})
        poll_url = f"{BILI_QRCODE_POLL_URL}?{poll_params}"

        req = urllib.request.Request(
            poll_url,
            headers={"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"}
        )

        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                poll_data = json.loads(resp.read().decode())
        except Exception:
            time.sleep(2)
            continue

        status_code = poll_data.get("data", {}).get("code", -1)

        if status_code == 86101:
            # 未扫码
            if last_status != "waiting":
                print("  等待扫码...")
                last_status = "waiting"
        elif status_code == 86090:
            # 已扫码，待确认
            if last_status != "scanned":
                print("  已扫码，请在手机上确认登录...")
                last_status = "scanned"
        elif status_code == 86038:
            # 二维码过期
            print("  [ERROR] 二维码已过期，请重新运行")
            return None
        elif status_code == 0:
            # 登录成功！从响应头获取 Set-Cookie
            print("  登录成功！")

            # 从响应头提取Cookie
            cookies = resp.headers.get_all("Set-Cookie") or []
            cookie_dict = {}
            for cookie_str in cookies:
                # 只取 name=value 部分
                parts = cookie_str.split(";")
                for part in parts:
                    part = part.strip()
                    if "=" in part:
                        name, value = part.split("=", 1)
                        name = name.strip()
                        if name in ("SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMdKey"):
                            cookie_dict[name] = value

            # 也从 data.url 中提取 (备用方式)
            redirect_url = poll_data.get("data", {}).get("url", "")
            if redirect_url:
                parsed = urllib.parse.urlparse(redirect_url)
                params = urllib.parse.parse_qs(parsed.query)
                for key in ("SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMdKey"):
                    if key in params and key not in cookie_dict:
                        cookie_dict[key] = params[key][0]

            if "SESSDATA" not in cookie_dict:
                print("  [ERROR] 未能获取有效Cookie")
                return None

            # 保存为 Netscape 格式 (yt-dlp 兼容)
            return _save_cookies(cookie_dict)

        time.sleep(2)

    print("  [ERROR] 登录超时，请重新运行")
    return None


def _save_cookies(cookie_dict: dict) -> str:
    """将Cookie保存为 Netscape 格式文件"""
    lines = ["# Netscape HTTP Cookie File", ""]
    for name, value in cookie_dict.items():
        lines.append(f".bilibili.com\tTRUE\t/\tTRUE\t0\t{name}\t{value}")
    lines.append("")

    with open(COOKIES_FILE, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))

    print(f"  Cookie 已保存至: {COOKIES_FILE}")
    return COOKIES_FILE


def _check_cookies_valid(cookies_path: str) -> bool:
    """检查cookies文件是否有效（包含SESSDATA）"""
    if not cookies_path or not os.path.isfile(cookies_path):
        return False
    try:
        with open(cookies_path, "r", encoding="utf-8") as f:
            content = f.read()
        return "SESSDATA" in content
    except Exception:
        return False


# ── Cookie 检测 ───────────────────────────────────────────

def find_cookies() -> str | None:
    """自动查找B站 Cookie 文件"""
    candidates = [
        "cookies.txt",
        "bilibili_cookies.txt",
        os.path.join(os.path.dirname(__file__), "cookies.txt"),
        os.path.join(os.path.dirname(__file__), "bilibili_cookies.txt"),
    ]
    for path in candidates:
        if os.path.isfile(path):
            return path
    return None


# ── 交互式输入 ────────────────────────────────────────────

def prompt_input(label: str, default: str = "", optional: bool = False) -> str:
    """带默认值的交互式输入"""
    hint = f"  {label}"
    if default:
        hint += f" [{default}]"
    if optional:
        hint += " (回车跳过)"
    hint += ": "
    value = input(hint).strip()
    return value if value else default


def prompt_choice(label: str, options: list[tuple[str, str]], default_idx: int = 0) -> str:
    """交互式选择"""
    print(f"  {label}:")
    for i, (key, desc) in enumerate(options):
        marker = " >" if i == default_idx else "  "
        print(f"    {marker} {i + 1}. {desc}")
    while True:
        choice = input(f"  请选择 [{default_idx + 1}]: ").strip()
        if not choice:
            return options[default_idx][0]
        try:
            idx = int(choice) - 1
            if 0 <= idx < len(options):
                return options[idx][0]
        except ValueError:
            pass
        print(f"  无效选择，请输入 1-{len(options)}")


def interactive_mode(preset_url: str = None) -> dict:
    """交互式引导用户输入所有参数"""
    print()
    print("=" * 50)
    print("  B站高清视频下载工具 (1080P / 4K)")
    print("  (交互模式)")
    print("=" * 50)
    print()

    # 1. 视频URL
    if preset_url:
        url = preset_url
        print(f"  视频地址: {url}")
    else:
        url = prompt_input("视频地址 (BV号/av号/链接)")
        if not url:
            print("[ERROR] 必须输入视频地址")
            sys.exit(1)

    # 2. 下载画质
    quality = prompt_choice(
        "下载画质",
        [("1080P", "1080P 高清 (需登录Cookie)"),
         ("4K",    "4K 超高清 (需大会员Cookie)")],
        default_idx=0
    )

    # 3. Cookie / 登录
    auto_cookies = find_cookies()
    if auto_cookies and _check_cookies_valid(auto_cookies):
        login_choice = prompt_choice(
            "登录方式",
            [("cached", f"使用已有Cookie ({os.path.basename(auto_cookies)})"),
             ("qrcode", "扫码登录 (获取新Cookie)"),
             ("file",   "指定Cookie文件")],
            default_idx=0
        )
    else:
        login_choice = prompt_choice(
            "登录方式 (1080P/4K需要登录)",
            [("qrcode", "扫码登录 (推荐)"),
             ("file",   "指定Cookie文件"),
             ("skip",   "跳过 (仅能下载低画质)")],
            default_idx=0
        )

    cookies = None
    if login_choice == "qrcode":
        cookies = qr_login()
        if not cookies:
            print("  [WARN] 登录失败，将尝试不登录下载")
    elif login_choice == "file":
        cookies = prompt_input("Cookie 文件路径")
        if not cookies:
            cookies = None
    elif login_choice == "cached":
        cookies = auto_cookies
    # skip: cookies = None

    # 4. 裁剪设置
    print()
    need_clip = prompt_choice(
        "是否裁剪片段",
        [("no", "不裁剪，下载完整视频"),
         ("yes", "裁剪片段")],
        default_idx=0
    )

    start = None
    end = None
    duration = None

    if need_clip == "yes":
        clip_mode = prompt_choice(
            "裁剪方式",
            [("duration", "指定起始时间 + 时长"),
             ("end", "指定起始时间 + 结束时间")],
            default_idx=0
        )

        start_str = prompt_input("起始时间 (秒 或 MM:SS)", default="0")
        start = parse_time(start_str)

        if clip_mode == "duration":
            dur_str = prompt_input("截取时长 (秒)", default="10")
            duration = float(dur_str)
        else:
            end_str = prompt_input("结束时间 (秒 或 MM:SS)")
            if end_str:
                end = parse_time(end_str)

    # 5. 输出格式
    fmt = prompt_choice(
        "输出格式",
        [("mp4", "MP4 (推荐，兼容性最好)"),
         ("mkv", "MKV (保留最高画质，支持多音轨)")],
        default_idx=0
    )

    # 6. 输出目录
    output_dir = prompt_input("输出目录", default="./downloads")

    return {
        "url": url,
        "quality": quality,
        "start": start,
        "end": end,
        "duration": duration,
        "format": fmt,
        "output": output_dir,
        "cookies": cookies,
    }


# ── 下载 ──────────────────────────────────────────────────

def get_video_info(yt_dlp: str, url: str, cookies: str = None) -> dict:
    """获取视频信息"""
    info_cmd = [yt_dlp, "-j", "--no-warnings", url]
    if cookies:
        info_cmd.extend(["--cookies", cookies])

    try:
        result = subprocess.run(info_cmd, capture_output=True, text=True, timeout=60)
        if result.returncode == 0:
            return json.loads(result.stdout)
    except Exception:
        pass
    return {}


def download_video(yt_dlp: str, url: str, output_dir: str, quality: str,
                   cookies: str = None, output_format: str = "mp4") -> tuple[str, str, dict]:
    """下载视频，返回 (文件路径, 标题, 视频信息)"""
    print(f"\n[1/2] 下载视频...")

    # 获取视频信息
    info = get_video_info(yt_dlp, url, cookies)
    title = info.get("title", "video")
    video_duration = info.get("duration", 0)
    print(f"  标题: {title}")
    if video_duration:
        print(f"  时长: {format_time_str(video_duration)}")

    # 清理文件名中的非法字符
    safe_title = re.sub(r'[<>:"/\\|?*～]', '_', title)
    safe_title = safe_title.strip('. ')

    # 构建下载命令
    output_template = os.path.join(output_dir, "download.%(ext)s")
    merge_format = "mkv" if output_format == "mkv" else "mp4"

    fmt_selector = QUALITY_FORMAT.get(quality, QUALITY_FORMAT["1080P"])

    cmd = [
        yt_dlp,
        "-o", output_template,
        "-f", fmt_selector,
        "--merge-output-format", merge_format,
        "--no-warnings",
        "--progress",
        url
    ]
    if cookies:
        cmd.extend(["--cookies", cookies])

    print(f"  画质: {quality}")
    print()
    result = subprocess.run(cmd)

    if result.returncode != 0:
        print(f"\n  [WARN] 指定画质下载失败，尝试默认画质...")
        cmd_simple = [
            yt_dlp,
            "-o", output_template,
            "--merge-output-format", merge_format,
            "--no-warnings",
            "--progress",
            url
        ]
        if cookies:
            cmd_simple.extend(["--cookies", cookies])
        result = subprocess.run(cmd_simple)
        if result.returncode != 0:
            print(f"[ERROR] 下载失败")
            sys.exit(1)

    # 查找下载的文件
    downloaded = None
    for f in os.listdir(output_dir):
        if f.startswith("download."):
            downloaded = os.path.join(output_dir, f)
            break

    if not downloaded or not os.path.exists(downloaded):
        print("[ERROR] 下载文件未找到")
        sys.exit(1)

    size_mb = os.path.getsize(downloaded) / (1024 * 1024)
    print(f"\n  下载完成: {size_mb:.2f} MB")

    return downloaded, safe_title, info


# ── 裁剪 ──────────────────────────────────────────────────

def clip_video(ffmpeg: str, input_file: str, output_file: str,
               start: float = None, end: float = None, duration: float = None) -> str:
    """裁剪视频片段 (无损，不重新编码)"""
    has_clip = start is not None or end is not None or duration is not None

    if not has_clip:
        shutil.copy2(input_file, output_file)
        return output_file

    print(f"\n[2/2] 裁剪片段 (无损)...")

    # 使用 -ss 在 -i 之前实现快速 seek
    cmd = [ffmpeg, "-y"]

    if start is not None and start > 0:
        cmd.extend(["-ss", format_time_str(start)])
        print(f"  起始: {format_time_str(start)}")

    cmd.extend(["-i", input_file])

    if end is not None:
        cmd.extend(["-to", format_time_str(end)])
        print(f"  结束: {format_time_str(end)}")
    elif duration is not None:
        cmd.extend(["-t", format_time_str(duration)])
        print(f"  时长: {format_time_str(duration)}")

    cmd.extend(["-c", "copy", output_file])

    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        # 无损裁剪失败时，尝试重新编码裁剪
        print(f"  [WARN] 无损裁剪失败，尝试重新编码裁剪...")
        cmd_reencode = [ffmpeg, "-y", "-i", input_file]
        if start is not None and start > 0:
            cmd_reencode.extend(["-ss", format_time_str(start)])
        if end is not None:
            cmd_reencode.extend(["-to", format_time_str(end)])
        elif duration is not None:
            cmd_reencode.extend(["-t", format_time_str(duration)])
        cmd_reencode.extend([
            "-c:v", "libx264", "-preset", "medium", "-crf", "18",
            "-c:a", "aac", "-b:a", "192k",
            output_file
        ])
        result = subprocess.run(cmd_reencode, capture_output=True, text=True)
        if result.returncode != 0:
            print(f"[ERROR] 裁剪失败: {result.stderr[-300:]}")
            sys.exit(1)

    size_mb = os.path.getsize(output_file) / (1024 * 1024)
    print(f"  裁剪完成: {size_mb:.2f} MB")

    return output_file


# ── 核心流程 ──────────────────────────────────────────────

def run(params: dict, yt_dlp: str, ffmpeg: str):
    """执行下载+裁剪"""
    url = normalize_url(params["url"])
    output_dir = os.path.abspath(params["output"])
    os.makedirs(output_dir, exist_ok=True)

    start = params.get("start")
    end = params.get("end")
    duration = params.get("duration")
    fmt = params.get("format", "mp4")
    quality = params.get("quality", "1080P")
    cookies = params.get("cookies")

    print()
    print("=" * 50)
    print("  开始处理")
    print("=" * 50)
    print(f"  URL:    {url}")
    print(f"  画质:   {quality}")
    print(f"  格式:   {fmt}")
    if start is not None:
        print(f"  起始:   {format_time_str(start)}")
    if end is not None:
        print(f"  结束:   {format_time_str(end)}")
    if duration is not None:
        print(f"  时长:   {format_time_str(duration)}")
    print(f"  输出:   {output_dir}")
    print("=" * 50)

    with tempfile.TemporaryDirectory(prefix="bili_dl_") as tmp_dir:
        # Step 1: 下载
        downloaded, title, info = download_video(yt_dlp, url, tmp_dir, quality, cookies, fmt)

        # Step 2: 裁剪 (如果需要)
        has_clip = start is not None or end is not None or duration is not None
        ext = os.path.splitext(downloaded)[1]

        if has_clip:
            clipped = os.path.join(tmp_dir, f"clipped{ext}")
            clip_video(ffmpeg, downloaded, clipped, start, end, duration)
            source = clipped
            suffix = "_clip"
        else:
            source = downloaded
            suffix = ""

        # 复制到输出目录
        output_file = os.path.join(output_dir, f"{title}{suffix}{ext}")
        shutil.copy2(source, output_file)

    # 输出结果
    print()
    print("=" * 50)
    print("  下载完成！")
    print("=" * 50)
    size_mb = os.path.getsize(output_file) / (1024 * 1024)
    print(f"  {os.path.basename(output_file):50s} {size_mb:8.2f} MB")
    print(f"  路径: {output_file}")
    print("=" * 50)

    # 打开输出目录
    try:
        if sys.platform == "win32":
            os.startfile(output_dir)
        elif sys.platform == "darwin":
            subprocess.run(["open", output_dir])
        else:
            subprocess.run(["xdg-open", output_dir])
    except Exception:
        pass


# ── 主入口 ────────────────────────────────────────────────

def main():
    # 检测是否有 --cli 参数（纯命令行模式）
    if "--cli" in sys.argv:
        sys.argv.remove("--cli")
        cli_mode = True
    else:
        cli_mode = False

    # 检测依赖
    yt_dlp, ffmpeg = check_dependencies()

    if cli_mode:
        # ── 纯命令行模式 ──
        parser = argparse.ArgumentParser(
            description="B站高清视频下载工具 (1080P / 4K)",
            formatter_class=argparse.RawDescriptionHelpFormatter,
            epilog="""
示例:
  %(prog)s BV17d4y1u78H                          下载完整视频 (1080P)
  %(prog)s BV17d4y1u78H -q 4K                    下载4K画质
  %(prog)s BV17d4y1u78H --start 5 --duration 10  从第5秒截取10秒
  %(prog)s BV17d4y1u78H --login                  扫码登录后下载
  %(prog)s --login-only                           仅扫码登录，保存Cookie
  %(prog)s BV17d4y1u78H --cookies cookies.txt    使用Cookie文件下载
            """
        )
        parser.add_argument("url", nargs="?", help="B站视频 BV号/av号/链接")
        parser.add_argument("--start", default=None, help="起始时间 (秒 或 HH:MM:SS)")
        parser.add_argument("--end", default=None, help="结束时间 (秒 或 HH:MM:SS)")
        parser.add_argument("--duration", default=None, help="截取时长 (秒)")
        parser.add_argument("-f", "--format", default="mp4",
                            choices=["mp4", "mkv"],
                            help="输出格式 (默认: mp4)")
        parser.add_argument("-q", "--quality", default="1080P",
                            choices=["1080P", "4K"],
                            help="下载画质 (默认: 1080P)")
        parser.add_argument("--login", action="store_true", help="扫码登录B站")
        parser.add_argument("--login-only", action="store_true", help="仅扫码登录并保存Cookie，不下载")
        parser.add_argument("--cookies", default=None, help="B站Cookie文件")
        parser.add_argument("-o", "--output", default="./downloads", help="输出目录")

        args = parser.parse_args()

        if args.end and args.duration:
            parser.error("--end 和 --duration 不能同时使用")

        # 仅登录模式
        if args.login_only:
            cookies = qr_login()
            if cookies:
                print(f"\n  Cookie 已保存，后续下载将自动使用")
            else:
                print("\n  登录失败")
            return

        if not args.url:
            parser.error("请提供视频URL，或使用 --login-only 仅登录")

        # 处理登录
        cookies = args.cookies
        if args.login and not cookies:
            cookies = qr_login()

        # 自动查找已有Cookie
        if not cookies:
            auto = find_cookies()
            if auto and _check_cookies_valid(auto):
                cookies = auto

        params = {
            "url": args.url,
            "quality": args.quality,
            "start": parse_time(args.start) if args.start else None,
            "end": parse_time(args.end) if args.end else None,
            "duration": float(args.duration) if args.duration else None,
            "format": args.format,
            "output": args.output,
            "cookies": cookies,
        }
    else:
        # ── 交互式模式 ──
        preset_url = None
        if len(sys.argv) > 1 and not sys.argv[1].startswith("-"):
            preset_url = sys.argv[1]

        params = interactive_mode(preset_url)

    # 执行
    run(params, yt_dlp, ffmpeg)


if __name__ == "__main__":
    main()
