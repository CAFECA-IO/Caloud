// Caloud Design System — 前端共用函式與圖示
// 規格：docs/05-components.md §A.4。功能相同的邏輯不得在頁面中重寫。
"use strict";

const UI = (() => {
  const $ = (s, root = document) => root.querySelector(s);
  const $$ = (s, root = document) => Array.from(root.querySelectorAll(s));

  /** HTML 轉義：所有插入 innerHTML 的動態值都必須經過 */
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

  /** 呼叫後端；非 2xx 時 throw Error(伺服器訊息) */
  async function api(path, opts = {}) {
    let r;
    try {
      r = await fetch(path, { credentials: "same-origin", headers: { "content-type": "application/json" }, ...opts, body: opts.body && JSON.stringify(opts.body) });
    } catch {
      throw new Error("無法連線到伺服器，請檢查網路後再試。");
    }
    if (r.status === 204 || r.status === 202) return null;
    const ct = r.headers.get("content-type") || "";
    const data = ct.includes("json") ? await r.json() : await r.text();
    if (!r.ok) {
      const e = new Error((data && data.error) || (typeof data === "string" && data) || "伺服器錯誤，請稍後再試。");
      e.status = r.status;
      throw e;
    }
    return data;
  }

  /** Unix 秒 → 完整本地時間 */
  const ts = (t) => new Date(t * 1000).toLocaleString("zh-TW", { hour12: false });

  /** Unix 秒 → 相對時間（「3 分鐘前」）；完整時間放在 title */
  function relTime(t) {
    const diff = Math.round(Date.now() / 1000 - t);
    const rtf = new Intl.RelativeTimeFormat("zh-TW", { numeric: "auto" });
    const units = [["year", 31536000], ["month", 2592000], ["day", 86400], ["hour", 3600], ["minute", 60]];
    for (const [u, s] of units) if (Math.abs(diff) >= s) return `<time datetime="${new Date(t * 1000).toISOString()}" title="${esc(ts(t))}">${rtf.format(-Math.round(diff / s), u)}</time>`;
    return `<time title="${esc(ts(t))}">剛剛</time>`;
  }

  // 狀態文字與色調：docs/04-design-language.md §6.3
  const STATUS = {
    app: { running: ["運作中", "success"], deploying: ["部署中", "warning", true], failed: ["失敗", "danger"], stopped: ["已停止", ""] },
    domain: { true: ["已驗證", "success"], false: ["待驗證", "warning"] },
    route: { live: ["轉發中", "success"], idle: ["App 未運作", "warning"], none: ["未綁定", ""] },
    tls: { issued: ["已簽發", "success"], pending: ["申請中", "warning"], failed: ["失敗", "danger"], none: ["未啟用", ""] },
    role: { super_admin: ["Super Admin", "accent"], admin: ["Admin", "accent"], member: ["Member", ""], pending: ["待核准", "warning"], disabled: ["已停用", "danger"] },
  };
  /** 產生 Badge（StatusPill）HTML；kind 為 app／domain／route／tls／role */
  function statusPill(kind, value) {
    const [label, tone, pulse] = STATUS[kind][String(value)] || [String(value), ""];
    const cls = ["c-badge", tone && `c-badge--${tone}`, pulse && "c-badge--pulse"].filter(Boolean).join(" ");
    return `<span class="${cls}"><span class="c-badge__dot" aria-hidden="true"></span>${esc(label)}</span>`;
  }

  /** 在 InlineMessage 顯示錯誤；err 為 null 時清除 */
  function showMsg(el, err) {
    el.textContent = err ? err.code || err.message || String(err) : "";
  }

  /** Toast：成功或失敗的短暫回饋（4 秒） */
  function toast(message, tone = "success") {
    let region = $(".c-toast-region");
    if (!region) {
      region = document.createElement("div");
      region.className = "c-toast-region";
      region.setAttribute("role", "status");
      region.setAttribute("aria-live", "polite");
      document.body.append(region);
    }
    const el = document.createElement("div");
    el.className = `c-toast c-toast--${tone}`;
    el.innerHTML = `${icon(tone === "danger" ? "alert" : "check")}<span>${esc(message)}</span>`;
    region.append(el);
    setTimeout(() => el.remove(), 4000);
  }

  /** ConfirmDialog：回傳 Promise<boolean>。不得使用 window.confirm */
  function confirmDialog({ title, body, confirmText, tone = "danger" }) {
    const d = document.createElement("dialog");
    d.className = "c-dialog";
    d.innerHTML = `<div class="c-dialog__body"><h2 class="c-dialog__title">${esc(title)}</h2><p class="u-muted">${esc(body)}</p></div>
      <div class="c-dialog__actions"><button class="c-btn" data-v="0">取消</button><button class="c-btn c-btn--primary ${tone === "danger" ? "c-btn--danger" : ""}" data-v="1">${esc(confirmText)}</button></div>`;
    document.body.append(d);
    return new Promise((resolve) => {
      const done = (v) => { d.close(); d.remove(); resolve(v); };
      d.addEventListener("click", (e) => { const v = e.target.closest("[data-v]"); if (v) done(v.dataset.v === "1"); });
      d.addEventListener("cancel", (e) => { e.preventDefault(); done(false); });
      d.showModal();
      $("[data-v='0']", d).focus();
    });
  }

  /** Drawer：從右側滑出的面板。回傳 { el, body, footer, close } */
  function drawer({ title, subtitle = "", onClose }) {
    const d = document.createElement("dialog");
    d.className = "c-drawer";
    d.setAttribute("aria-label", title);
    d.innerHTML = `<header class="c-drawer__header"><div class="c-drawer__heading">
        <h2 class="c-drawer__title u-truncate">${esc(title)}</h2>${subtitle ? `<div class="u-muted u-small">${subtitle}</div>` : ""}</div>
        <button class="c-btn c-btn--ghost c-btn--icon" data-close aria-label="關閉">${icon("x")}</button></header>
      <div class="c-drawer__body"></div><footer class="c-drawer__footer u-hidden"></footer>`;
    document.body.append(d);
    const close = () => { if (d.open) d.close(); };
    d.addEventListener("close", () => { d.remove(); onClose && onClose(); });
    d.addEventListener("click", (e) => { if (e.target === d || e.target.closest("[data-close]")) close(); });
    d.showModal();
    return { el: d, body: $(".c-drawer__body", d), footer: $(".c-drawer__footer", d), close };
  }

  /** 執行期間停用按鈕並顯示「處理中…」 */
  async function busy(button, promise) {
    if (!button) return promise;
    const html = button.innerHTML;
    button.disabled = true;
    button.classList.add("is-loading");
    button.innerHTML = `${icon("loader")}<span>處理中…</span>`;
    try { return await promise; } finally { button.disabled = false; button.classList.remove("is-loading"); button.innerHTML = html; }
  }

  /** 複製文字到剪貼簿並以 Toast 回饋 */
  async function copy(text, label = "內容") {
    try { await navigator.clipboard.writeText(text); toast(`已複製${label}`); }
    catch { toast("瀏覽器不允許自動複製，請手動選取。", "danger"); }
  }

  /** CopyField HTML */
  const copyField = (value, label) =>
    `<div class="c-copy"><span class="c-copy__value">${esc(value)}</span><button class="c-btn c-btn--ghost c-btn--icon c-btn--sm" data-copy="${esc(value)}" data-copy-label="${esc(label || "")}" aria-label="複製${esc(label || "")}">${icon("copy")}</button></div>`;
  document.addEventListener("click", (e) => { const b = e.target.closest("[data-copy]"); if (b) copy(b.dataset.copy, b.dataset.copyLabel); });

  /** EmptyState HTML */
  const empty = ({ iconName = "inbox", title, body = "", action = "" }) =>
    `<div class="c-empty">${icon(iconName)}<div class="c-empty__title">${esc(title)}</div>${body ? `<p>${esc(body)}</p>` : ""}${action}</div>`;

  /** 表格載入中的 Skeleton 列 */
  const skeletonRows = (cols, rows = 3) =>
    Array.from({ length: rows }, () => `<tr>${Array.from({ length: cols }, () => `<td><span class="c-skeleton"></span></td>`).join("")}</tr>`).join("");

  // 圖示：24×24 線條圖示，stroke 跟隨文字色。新增圖示需同步更新 docs/05 §A.5
  const ICONS = {
    home: '<path d="M3 11l9-7 9 7"/><path d="M5 10v10h14V10"/>',
    box: '<path d="M21 8l-9-5-9 5 9 5 9-5z"/><path d="M3 8v8l9 5 9-5V8"/><path d="M12 13v8"/>',
    globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3c3 3.5 3 14.5 0 18M12 3c-3 3.5-3 14.5 0 18"/>',
    users: '<circle cx="9" cy="8" r="3.5"/><path d="M2.5 20c.8-3.5 3.4-5.5 6.5-5.5s5.7 2 6.5 5.5"/><path d="M16 4.5a3.5 3.5 0 010 7M18 14.8c1.8.7 3 2.5 3.5 5.2"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    search: '<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>',
    copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 00-1-1H5a1 1 0 00-1 1v10a1 1 0 001 1h3"/>',
    check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
    x: '<path d="M6 6l12 12M18 6L6 18"/>',
    play: '<path d="M7 5l12 7-12 7V5z"/>',
    stop: '<rect x="6" y="6" width="12" height="12" rx="1.5"/>',
    refresh: '<path d="M20 11a8 8 0 10-2.3 5.7"/><path d="M20 4v7h-7"/>',
    trash: '<path d="M4 7h16M10 11v6M14 11v6"/><path d="M6 7l1 13h10l1-13M9 7V4h6v3"/>',
    logout: '<path d="M15 4h4v16h-4"/><path d="M10 8l-4 4 4 4M6 12h10"/>',
    alert: '<path d="M12 3l9.5 17h-19L12 3z"/><path d="M12 10v4M12 17.5v.5"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5M12 7.5v.5"/>',
    inbox: '<path d="M3 13l3-8h12l3 8v6H3v-6z"/><path d="M3 13h5l1 2h6l1-2h5"/>',
    link: '<path d="M10 14a4 4 0 005.7 0l3-3a4 4 0 00-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 00-5.7 0l-3 3a4 4 0 005.7 5.7l1-1"/>',
    upload: '<path d="M12 16V4M7 9l5-5 5 5"/><path d="M4 16v4h16v-4"/>',
    loader: '<path d="M12 3a9 9 0 109 9"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M2 12h2M20 12h2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5-1.5M17.5 6.5L19 5"/>',
    moon: '<path d="M20 14.5A8 8 0 019.5 4a8 8 0 1010.5 10.5z"/>',
    key: '<circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3M15 8l2 2"/>',
    shield: '<path d="M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6l8-3z"/>',
  };
  /** 圖示 SVG 字串；裝飾用時 aria-hidden */
  const icon = (name) => `<svg class="c-icon" viewBox="0 0 24 24" aria-hidden="true">${ICONS[name] || ""}</svg>`;

  /** 主題（淺色／深色／跟隨系統），記在本機 */
  function initTheme() {
    let t = null;
    try { t = localStorage.getItem("caloud.theme"); } catch {}
    if (t === "light" || t === "dark") document.documentElement.dataset.theme = t;
  }
  function toggleTheme() {
    const root = document.documentElement;
    const isDark = root.dataset.theme ? root.dataset.theme === "dark" : matchMedia("(prefers-color-scheme: dark)").matches;
    root.dataset.theme = isDark ? "light" : "dark";
    try { localStorage.setItem("caloud.theme", root.dataset.theme); } catch {}
  }


  return { $, $$, esc, api, ts, relTime, statusPill, showMsg, toast, confirmDialog, drawer, busy, copy, copyField, empty, skeletonRows, icon, initTheme, toggleTheme };
})();
