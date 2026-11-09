import os
from lib import Svg, tw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
s = Svg(980, 470, "App 部署與流量轉發", "上傳的 compose 檔先經安全檢查，再加上 Caloud 的 override 後以 docker compose 啟動；部署完成後路由表即時更新，訪客的請求依 Host 轉發到容器。")
s.text(24, 38, "App 部署與流量轉發", "b", 18)
s.text(24, 70, "部署", "b m", 13)
steps = [
    ("上傳 compose", ["網頁、REST 或 MCP", "status：stopped"]),
    ("安全檢查", ["禁止 ports、privileged、", "主機路徑、build…"]),
    ("產生 override", ["加入 caloud-edge 網路", "alias、資源上限"]),
    ("docker compose up", ["-d --wait", "status：deploying"]),
    ("更新狀態與路由", ["running／failed", "路由表即時重載"]),
]
w, gap, x, y = 172, 18, 24, 84
for i, (title, sub) in enumerate(steps, 1):
    cls = "okbg" if i == 5 else "box"
    s.rect(x, y, w, 92, cls)
    s.num(x + 22, y + 24, i)
    s.text(x + 40, y + 29, title, "b", 13)
    s.lines(x + 16, y + 56, sub, 12, 19, "m s mono" if i == 4 else "m s")
    if i < 5:
        s.line([(x + w, y + 46), (x + w + gap, y + 46)])
    x += w + gap
s.rect(180, 190, 240, 34, "warnbg", 6)
s.text(300, 212, "不合規 → 400，指出 service 與欄位", "s warn", 11, "middle")
s.line([(300, 176), (300, 190)], "ln")

# 轉發
s.text(24, 262, "轉發", "b m", 13)
y = 276
s.rect(24, y, 160, 64, "box"); s.text(40, y + 27, "訪客", "b", 13); s.text(40, y + 48, "blog.example.com", "m s mono", 11)
s.rect(232, y, 300, 64, "acc"); s.text(248, y + 27, "Caloud 反向代理", "b acct", 13)
s.text(248, y + 48, "Host → http://caloud-<id>-web:80", "m s mono", 11)
s.rect(580, y - 12, 376, 160, "soft", 10); s.text(596, y + 12, "caloud-edge 網路（不對主機開 port）", "m s", 12)
s.rect(596, y + 24, 160, 56, "box"); s.text(612, y + 48, "web", "b mono", 13); s.text(612, y + 68, "alias：caloud-<id>-web", "m s mono", 10)
s.rect(780, y + 24, 160, 56, "box"); s.text(796, y + 48, "db", "b mono", 13); s.text(796, y + 68, "只在 App 內部網路", "m s", 11)
s.line([(184, y + 32), (232, y + 32)], "lna", "aa")
s.line([(532, y + 32), (596, y + 52)], "lna", "aa")
s.line([(756, y + 52), (780, y + 52)], "ln", "a")
s.rect(24, 372, 508, 74, "soft", 8)
s.lines(40, 396, ["找不到路由：404「此網域尚未綁定運作中的 App」", "容器沒有回應：502「App 沒有回應，請稍後再試」", "只有「已驗證＋已綁定＋App 運作中」的網域會進入路由表"], 12, 20, "m s")
s.save(os.path.join(OUT, "deploy-flow.svg"))
