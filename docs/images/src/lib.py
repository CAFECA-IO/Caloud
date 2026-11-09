# README 圖示的共用繪圖工具：顏色取自 docs/04-design-language.md（企業儀表板），以 prefers-color-scheme 支援深色
from xml.sax.saxutils import escape as x
STYLE = '''<style>
  .bg{fill:#ffffff}
  .t{font-family:system-ui,-apple-system,"Segoe UI","Noto Sans TC","PingFang TC","Microsoft JhengHei",sans-serif;fill:#111827}
  .m{fill:#4b5563} .s{font-size:12px} .b{font-weight:600} .mono{font-family:ui-monospace,"SF Mono",Menlo,Consolas,monospace}
  .box{fill:#ffffff;stroke:#cfd4dc;stroke-width:1.25}
  .soft{fill:#f6f7f9;stroke:#e4e7ec;stroke-width:1.25}
  .grp{fill:none;stroke:#cfd4dc;stroke-width:1.25;stroke-dasharray:5 4}
  .acc{fill:#eef3ff;stroke:#1d4ed8;stroke-width:1.5}
  .acct{fill:#1d4ed8} .ok{fill:#15803d} .okbg{fill:#ecfdf3;stroke:#15803d;stroke-width:1.25}
  .warnbg{fill:#fef6e7;stroke:#a15c07;stroke-width:1.25} .warn{fill:#a15c07}
  .dark{fill:#0f172a} .darkt{fill:#ffffff}
  .ln{stroke:#5f6774;stroke-width:1.5;fill:none} .lna{stroke:#1d4ed8;stroke-width:1.75;fill:none}
  .life{stroke:#cfd4dc;stroke-width:1.25;stroke-dasharray:4 4}
  .dot{fill:#1d4ed8} .num{fill:#ffffff;font-weight:600;font-size:12px}
  .ah{fill:#5f6774} .aha{fill:#1d4ed8}
  @media (prefers-color-scheme: dark){
    .bg{fill:#0b0f17} .t{fill:#e6e9ef} .m{fill:#a3acb9}
    .box{fill:#111722;stroke:#334052} .soft{fill:#18202d;stroke:#222b38} .grp{stroke:#334052}
    .acc{fill:#14223d;stroke:#6ea2ff} .acct{fill:#6ea2ff} .ok{fill:#4ade80} .okbg{fill:#0f2a1b;stroke:#4ade80}
    .warnbg{fill:#2b2210;stroke:#fbbf24} .warn{fill:#fbbf24}
    .dark{fill:#070b12} .ln{stroke:#8c95a3} .lna{stroke:#6ea2ff} .life{stroke:#334052}
    .dot{fill:#6ea2ff} .num{fill:#08101f} .ah{fill:#8c95a3} .aha{fill:#6ea2ff}
  }
</style>
<defs>
  <marker id="a" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="ah" d="M0 0L10 5L0 10z"/></marker>
  <marker id="aa" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path class="aha" d="M0 0L10 5L0 10z"/></marker>
</defs>'''

class Svg:
    def __init__(s, w, h, title, desc):
        s.w, s.h, s.parts = w, h, []
        s.title, s.desc = title, desc
    def add(s, p): s.parts.append(p); return s
    def rect(s, x0, y0, w, h, cls="box", r=8):
        return s.add(f'<rect class="{cls}" x="{x0}" y="{y0}" width="{w}" height="{h}" rx="{r}"/>')
    def text(s, x0, y0, t, cls="", size=14, anchor="start", weight=None):
        st = f' font-size="{size}"' + (f' font-weight="{weight}"' if weight else "")
        return s.add(f'<text class="t {cls}" x="{x0}" y="{y0}"{st} text-anchor="{anchor}">{x(t)}</text>')
    def lines(s, x0, y0, items, size=13, gap=19, cls="", anchor="start"):
        for i, t in enumerate(items): s.text(x0, y0 + i * gap, t, cls, size, anchor)
        return s
    def line(s, pts, cls="ln", arrow="a", start=False):
        d = "M" + " L".join(f"{a} {b}" for a, b in pts)
        ms = f' marker-start="url(#{arrow})"' if start else ""
        return s.add(f'<path class="{cls}" d="{d}" marker-end="url(#{arrow})"{ms}/>')
    def num(s, cx, cy, n):
        s.add(f'<circle class="dot" cx="{cx}" cy="{cy}" r="11"/>')
        return s.add(f'<text class="t num" x="{cx}" y="{cy + 4}" text-anchor="middle">{n}</text>')
    def save(s, path):
        body = "\n".join(s.parts)
        out = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {s.w} {s.h}" width="{s.w}" height="{s.h}" role="img" aria-labelledby="title desc">
<title id="title">{x(s.title)}</title>
<desc id="desc">{x(s.desc)}</desc>
{STYLE}
<rect class="bg" width="{s.w}" height="{s.h}" rx="12"/>
{body}
</svg>
'''
        open(path, "w").write(out)

def tw(t, size=12):
    """估計文字寬度（CJK 全形約 1em，其他約 0.56em）"""
    return sum(size if ord(c) > 0x2E80 else size * 0.56 for c in t)
