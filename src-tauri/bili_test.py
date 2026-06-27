import urllib.request, json

COOKIES_FILE = r"D:\pro_sunner\demo_vscode\SynthChat-V1.1.0\skills\media\bilibili-clip\scripts\bilibili_cookies.txt"

# Parse cookies from Netscape format
cookies = {}
with open(COOKIES_FILE, "r", encoding="utf-8") as f:
    for line in f:
        line = line.strip()
        if line.startswith("#") or not line or line == "":
            continue
        parts = line.split("\t")
        if len(parts) >= 7:
            cookies[parts[5]] = parts[6]

cookie_str = "; ".join(f"{k}={v}" for k, v in cookies.items())
print(f"Cookie keys: {list(cookies.keys())}")

req = urllib.request.Request(
    "https://api.bilibili.com/x/web-interface/nav",
    headers={"User-Agent": "Mozilla/5.0", "Cookie": cookie_str}
)
resp = urllib.request.urlopen(req, timeout=10)
data = json.loads(resp.read().decode())
is_login = data.get("data", {}).get("isLogin", False)
uname = data.get("data", {}).get("uname", "?")
code = data.get("code", -1)
print(f"code={code}, isLogin={is_login}, uname={uname}")
