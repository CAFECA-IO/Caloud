import os
from lib import Svg

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
s = Svg(980, 330, "網域設定四步驟", "新增網域、在 DNS 服務商設定 A 與 TXT 紀錄、由 Caloud 查詢 TXT 驗證擁有權，最後綁定 App 的 service 與 port，網域即開始轉發。")
s.text(24, 38, "網域設定四步驟", "b", 18)
cards = [
    ("新增網域", ["輸入主機名稱", "blog.example.com", "自動轉小寫、檢查格式"], "Caloud 產生驗證值"),
    ("設定 DNS", ["A  blog.example.com", "   → Caloud 主機 IP", "TXT _caloud-challenge.…", "   = caloud-4f1d…"], "在你的 DNS 服務商操作"),
    ("驗證擁有權", ["Caloud 以 DNS-over-HTTPS", "查詢 TXT 紀錄", "相符才標記「已驗證」"], "DNS 生效可能需數分鐘"),
    ("綁定 App", ["選擇 App 與 service", "填入容器內 port", "例：web : 80"], "立即生效，不需重啟"),
]
w, gap, x = 222, 20, 24
for i, (title, lines, foot) in enumerate(cards, 1):
    s.rect(x, 64, w, 196, "okbg" if i == 4 else "box")
    s.num(x + 24, 92, i)
    s.text(x + 44, 97, title, "b", 14)
    mono = i in (2,)
    s.lines(x + 18, 132, lines, 12, 22, "m s mono" if mono else "m s")
    s.add(f'<line class="life" x1="{x + 16}" y1="222" x2="{x + w - 16}" y2="222"/>')
    s.text(x + 18, 244, foot, "s", 12)
    if i < 4:
        s.line([(x + w, 162), (x + w + gap, 162)], "lna", "aa")
    x += w + gap
s.rect(24, 276, 932, 36, "soft", 8)
s.text(40, 299, "網頁的「新增網域」精靈與 MCP 工具 create_domain → verify_domain → bind_domain 都依這個順序；DNS 紀錄提供一鍵複製。", "s m", 12)
s.save(os.path.join(OUT, "domain-flow.svg"))
