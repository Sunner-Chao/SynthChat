import sys
import urllib.request
import json
import time
from urllib.parse import urlparse, parse_qs

QR_KEY = "0aafa3fd445dd19a7bd5464d6fdfc994"
POLL_URL = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll"
COOKIES_FILE = r"D:\pro_sunner\demo_vscode\SynthChat-V1.1.0\skills\media\bilibili-clip\scripts\bilibili_cookies.txt"

print("开始轮询扫码状态...")
start = time.time()
last_status = None
cookies_dict = {}

while time.time() - start < 180:
    req = urllib.request.Request(
        f"{POLL_URL}?qrcode_key={QR_KEY}",
        headers={"User-Agent": "Mozilla/5.0"}
    )
    try:
        resp = urllib.request.urlopen(req, timeout=10)
        poll_data = json.loads(resp.read().decode())
        code = poll_data.get("data", {}).get("code", -1)

        if code == 86101 and last_status != "waiting":
            print("等待扫码...")
            last_status = "waiting"
        elif code == 86090 and last_status != "scanned":
            print("已扫码，请在手机上确认登录...")
            last_status = "scanned"
        elif code == 86038:
            print("二维码已过期")
            break
        elif code == 0:
            print("登录成功！")
            redirect_url = poll_data.get("data", {}).get("url", "")
            if redirect_url:
                params = parse_qs(urlparse(redirect_url).query)
                for k in ["SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMdKey"]:
                    if k in params:
                        cookies_dict[k] = params[k][0]

            if cookies_dict:
                lines = ["# Netscape HTTP Cookie File", ""]
                for k, v in cookies_dict.items():
                    lines.append(f".bilibili.com\tTRUE\t/\tTRUE\t0\t{k}\t{v}")
                lines.append("")
                with open(COOKIES_FILE, "w", encoding="utf-8") as f:
                    f.write("\n".join(lines))
                print(f"COOKIES_SAVED={COOKIES_FILE}")
            else:
                print("未能获取有效Cookie")
            break
    except Exception as e:
        pass
    time.sleep(2)
else:
    print("登录超时")
