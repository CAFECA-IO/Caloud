import os
from lib import Svg, tw

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
s = Svg(980, 600, "Sign in with CAFECA 登入流程", "瀏覽器向 Caloud 取得一次性 nonce，CAFECA 錢包以 Passkey 簽署綁定網域的 EIP-712 訊息，Caloud 以公開 RPC 呼叫身分合約 isValidSignature 驗證後建立 session；第一位登入者成為 Super Admin。")
s.text(24, 38, "Sign in with CAFECA 登入流程", "b", 18)
cols = {"b": (130, "瀏覽器", "管理者"), "c": (390, "Caloud", "後端"), "w": (650, "CAFECA 錢包", "Passkey"), "r": (880, "公開 RPC", "身分合約")}
for k, (x0, name, sub) in cols.items():
    cls = "acc" if k == "c" else "box"
    s.rect(x0 - 70, 60, 140, 52, cls)
    s.text(x0, 83, name, "b" + (" acct" if k == "c" else ""), 14, "middle")
    s.text(x0, 102, sub, "m s", 12, "middle")
    s.add(f'<line class="life" x1="{x0}" y1="112" x2="{x0}" y2="540"/>')
X = {k: v[0] for k, v in cols.items()}
steps = [
    ("b", "c", "POST /api/auth/nonce", None),
    ("c", "b", "nonce（128 bit，5 分鐘，只能用一次）", None),
    ("b", "w", "signIn：網域、nonce、有效期", None),
    ("w", "w", "使用者以指紋／Passkey 簽署 EIP-712 SignIn", None),
    ("w", "b", "account（身分合約）＋ signature", None),
    ("b", "c", "POST /api/auth/login（原封不動轉交）", None),
    ("c", "c", "作廢 nonce；檢查網域、chainId、有效期；計算 SignIn 雜湊", None),
    ("c", "r", "eth_call isValidSignature(hash, sig)", None),
    ("r", "c", "0x1626ba7e（簽章有效）", None),
    ("c", "b", "session cookie（HttpOnly、SameSite=Strict）", None),
]
y = 150
for i, (a, b, label, _) in enumerate(steps, 1):
    xa, xb = X[a], X[b]
    if a == b:
        w = tw(label) + 24
        x0 = xa + 14 if a != "w" else xa - w / 2
        x0 = min(x0, 980 - w - 12)
        s.rect(round(x0), y - 18, round(w), 30, "soft", 6)
        s.num(round(x0) - 16, y - 3, i)
        s.text(x0 + 12, y + 2, label, "s", 12)
    else:
        cls, arrow = ("lna", "aa") if a == "b" or b == "r" else ("ln", "a")
        s.line([(xa, y), (xb + (-6 if xb > xa else 6), y)], cls, arrow)
        mid = (xa + xb) / 2
        s.text(mid, y - 8, label, "s", 12, "middle")
        s.num(round(mid - tw(label) / 2 - 16), y - 12, i)
    y += 38
s.rect(260, y - 16, 460, 44, "okbg", 8)
s.text(490, y + 4, "建立或更新使用者：資料庫中沒有任何使用者時，此人成為 Super Admin", "s ok", 12, "middle")
s.text(490, y + 21, "其餘新使用者預設為「待核准」", "s m", 12, "middle")
s.save(os.path.join(OUT, "signin-flow.svg"))
