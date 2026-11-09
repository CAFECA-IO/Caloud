import os
from lib import Svg

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
s = Svg(980, 530, "Caloud 系統架構", "管理者、AI 助手與網站訪客經由 Caloud 單一執行檔的 Web 介面、MCP 端點與反向代理，操作 Docker 中的 App 容器；登入由 CAFECA 錢包簽署並以公開 RPC 驗證。")
s.text(24, 38, "Caloud 系統架構", "b", 18)

# 左：使用者
users = [("管理者瀏覽器", "Web 介面、CAFECA 登入", 92), ("AI 助手", "MCP 用戶端（Claude 等）", 232), ("網站訪客", "app.example.com", 360)]
for name, sub, y in users:
    s.rect(24, y, 168, 64, "box")
    s.text(40, y + 27, name, "b", 14)
    s.text(40, y + 48, sub, "m s", 12)

# 中：Caloud
s.rect(244, 64, 404, 380, "acc", 12)
s.text(264, 92, "Caloud（單一執行檔）", "b acct", 15)
s.rect(264, 108, 176, 60, "box"); s.text(280, 133, "Web 介面＋REST API", "b", 13); s.text(280, 154, ":8080 · cookie", "m s mono", 11)
s.rect(264, 184, 176, 60, "box"); s.text(280, 209, "MCP 端點", "b", 13); s.text(280, 230, "POST /mcp · Bearer", "m s mono", 11)
s.rect(452, 108, 176, 136, "box"); s.text(468, 133, "服務層", "b", 13)
s.lines(468, 156, ["權限檢查", "compose 安全政策", "網域驗證", "稽核紀錄"], 12, 19, "m s")
s.line([(440, 138), (452, 138)]); s.line([(440, 214), (452, 214)])
s.rect(264, 268, 176, 60, "box"); s.text(280, 293, "SQLite", "b", 13); s.text(280, 314, "使用者 · App · 網域 · 權杖", "m s", 12)
s.rect(452, 268, 176, 60, "box"); s.text(468, 293, "Compose 執行器", "b", 13); s.text(468, 314, "docker compose CLI", "m s mono", 11)
s.line([(480, 244), (480, 256), (352, 256), (352, 268)]); s.line([(560, 244), (560, 268)])
s.rect(264, 352, 364, 72, "box"); s.text(280, 378, "反向代理", "b", 13); s.text(280, 400, ":80／:443 · 依 Host 查路由表；服務層變更後即時重載", "m s", 12)

# 使用者箭頭
s.line([(192, 124), (264, 138)], "lna", "aa")
s.line([(192, 264), (228, 264), (228, 214), (264, 214)], "lna", "aa")
s.line([(192, 392), (264, 388)], "lna", "aa")

# 右上：CAFECA
s.rect(700, 64, 256, 112, "soft", 10)
s.text(716, 90, "CAFECA", "b", 14)
s.rect(716, 102, 108, 58, "box"); s.text(728, 126, "錢包", "b", 13); s.text(728, 146, "Passkey 簽署", "m s", 12)
s.rect(836, 102, 108, 58, "box"); s.text(848, 126, "公開 RPC", "b", 13); s.text(848, 146, "ERC-1271 驗證", "m s", 12)
s.line([(628, 184), (890, 184), (890, 160)], "ln", "a")
s.text(651, 202, "驗證簽章", "m s", 12)
s.add('<path class="ln" d="M108 92 C108 48 150 50 244 50 L700 50 C760 50 770 70 770 102" marker-end="url(#a)" stroke-dasharray="5 4"/>')
s.text(420, 42, "登入時由錢包以 Passkey 簽署 EIP-712 SignIn", "m s", 12, "middle")

# 右下：Docker
s.rect(700, 196, 256, 300, "soft", 10)
s.text(716, 222, "Docker Engine", "b", 14)
s.rect(716, 236, 224, 248, "grp", 8); s.text(728, 258, "caloud-edge 網路", "m s mono", 12)
for i, (proj, svcs) in enumerate([("App：blog", "web · db"), ("App：api", "api · redis")]):
    y = 274 + i * 104
    s.rect(728, y, 200, 88, "box")
    s.text(744, y + 26, proj, "b", 13)
    s.text(744, y + 47, svcs, "m s mono", 12)
    s.text(744, y + 68, "獨立 compose project", "m s", 12)
s.line([(628, 298), (728, 310)], "ln", "a"); s.text(640, 290, "docker.sock", "m s mono", 11)
s.line([(628, 388), (728, 410)], "lna", "aa"); s.text(636, 380, "service:port", "m s mono", 11)
s.save(os.path.join(OUT, "architecture.svg"))
