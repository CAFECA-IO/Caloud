import os
from lib import Svg

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
s = Svg(980, 420, "MCP 介面運作方式", "使用者在網頁以 CAFECA 登入後建立個人 API 權杖，設定到 Claude 等 MCP 用戶端；AI 助手以 Bearer 權杖呼叫 POST /mcp，Caloud 依使用者目前角色驗證並經由共用服務層執行工具，所有操作寫入稽核。")
s.text(24, 38, "MCP 介面運作方式", "b", 18)
s.rect(24, 64, 260, 120, "box"); s.num(48, 92, 1); s.text(68, 97, "建立 API 權杖", "b", 14)
s.lines(40, 126, ["網頁 →「API 權杖」→ 建立", "cld_…  只顯示一次", "預設 90 天到期、可隨時撤銷"], 12, 20, "m s")
s.rect(24, 208, 260, 120, "box"); s.num(48, 236, 2); s.text(68, 241, "設定 MCP 用戶端", "b", 14)
s.lines(40, 270, ["claude mcp add --transport http", "  caloud https://…/mcp", '  --header "Authorization: Bearer cld_…"'], 11, 18, "m s mono")
s.line([(154, 184), (154, 208)])
s.rect(332, 136, 200, 120, "box"); s.num(356, 164, 3); s.text(376, 169, "AI 助手", "b", 14)
s.lines(348, 198, ["「部署這份 compose 並", "綁定 blog.example.com」", "→ POST /mcp（Bearer）"], 12, 20, "m s")
s.line([(284, 268), (308, 268), (308, 196), (332, 196)])
s.rect(580, 64, 376, 264, "acc", 12); s.text(600, 92, "Caloud", "b acct", 15)
s.line([(532, 196), (556, 196), (556, 136), (600, 136)], "lna", "aa")
boxes = [
    (112, "驗證權杖與來源", "Bearer 雜湊比對 · 拒絕 cookie · 檢查 Origin"),
    (172, "依目前角色授權", "pending／disabled 立即失效 · Admin 才有使用者工具"),
    (232, "共用服務層執行", "與網頁相同的權限、compose 政策與錯誤訊息"),
]
for y, t, sub in boxes:
    s.rect(600, y, 336, 48, "box")
    s.text(616, y + 21, t, "b", 13)
    s.text(616, y + 39, sub, "m s", 11)
s.line([(768, 160), (768, 172)]); s.line([(768, 220), (768, 232)])
s.rect(600, 288, 336, 28, "okbg", 6); s.text(616, 307, "稽核標示 via=mcp:<權杖名稱>；刪除需 confirm: true", "s ok", 11)
s.rect(24, 348, 932, 56, "soft", 8)
s.text(40, 371, "工具：whoami · list_apps · get_app · create_app · update_app · deploy_app · stop_app · delete_app · get_app_logs", "s m mono", 11)
s.text(82, 391, "list_domains · create_domain · verify_domain · bind_domain · delete_domain · Admin：list_users · set_user_role", "s m mono", 11)
s.save(os.path.join(OUT, "mcp-flow.svg"))
