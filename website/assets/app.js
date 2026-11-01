// 站点公用脚本：注入最新版本、复制按钮、平台推荐、更新日志渲染。
// 不依赖任何 CDN —— 包体积小、离线/CDN 被墙时也能正常用。

const REPO = "https://github.com/sodahub-org/sodam";
const RELEASES_JSON = "releases.json";

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

/* ---------------- 平台识别 ---------------- */

function detectPlatform() {
  const ua = navigator.userAgent.toLowerCase();
  const platform = (navigator.userAgentData?.platform || navigator.platform || "").toLowerCase();
  if (ua.includes("windows") || platform.includes("win")) return "windows";
  if (ua.includes("mac") || platform.includes("mac")) return "macos";
  if (ua.includes("linux") || platform.includes("linux")) return "linux";
  return "";
}

async function detectArch() {
  try {
    const ua = await navigator.userAgentData?.getHighEntropyValues?.(["architecture"]);
    const arch = (ua?.architecture || "").toLowerCase();
    if (arch) return arch === "arm" ? "aarch64" : "x86_64";
  } catch (_) {
    /* 忽略：退回默认值 */
  }
  // 非 Chromium 浏览器拿不到高熵值，按平台给默认架构
  return detectPlatform() === "macos" ? "aarch64" : "x86_64";
}

/* ---------------- 最新版本 ---------------- */

let releases = [];

async function loadReleases() {
  try {
    const res = await fetch(RELEASES_JSON, { cache: "no-cache" });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    releases = await res.json();
  } catch (err) {
    console.warn("读取 releases.json 失败：", err);
    releases = [];
  }
  return releases;
}

function latestTag() {
  return releases[0]?.tag || "";
}

function latestVersion() {
  return latestTag().replace(/^v/, "");
}

/** 给所有 [data-version] 填最新版本号 */
function fillVersion() {
  const version = latestVersion();
  if (!version) return;
  $$("[data-version]").forEach((el) => {
    el.textContent = el.dataset.version === "tag" ? latestTag() : version;
  });
}

/** 生成带版本的下载地址 */
function assetUrl(name) {
  const version = latestVersion();
  if (!version) return `${REPO}/releases/latest`;
  return `${REPO}/releases/download/v${version}/${name}`;
}

async function fillDownloadLinks() {
  const version = latestVersion();
  if (!version) return;
  const arch = await detectArch();
  const pkgArch = arch === "aarch64" ? "aarch64" : "x86_64";

  $$("[data-download]").forEach((el) => {
    const kind = el.dataset.download;
    let url = "";
    if (kind === "windows") url = assetUrl(`sodam-${version}-windows-x64-setup.exe`);
    if (kind === "macos") url = assetUrl(`sodam-${version}-macos-aarch64.zip`);
    if (kind === "linux") url = assetUrl(`sodam-${version}-1-${pkgArch}.pkg.tar.zst`);
    if (kind === "checksums") url = assetUrl("SHA256SUMS");
    if (url) el.href = url;
  });

  $$("[data-linux-arch]").forEach((el) => {
    el.textContent = pkgArch;
  });
}

/* ---------------- 复制按钮 ---------------- */

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch (_) {
    // 非 HTTPS / 老浏览器兜底
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.remove();
    return ok;
  }
}

function wireCopyButtons() {
  $$("[data-copy]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const target = btn.dataset.copy;
      const text = target === "self" ? btn.closest(".code")?.innerText : $(target)?.innerText;
      if (!text) return;
      const ok = await copyText(text.trim());
      const label = btn.textContent;
      btn.textContent = ok ? "已复制 ✓" : "复制失败";
      btn.classList.toggle("done", ok);
      setTimeout(() => {
        btn.textContent = label;
        btn.classList.remove("done");
      }, 1600);
    });
  });
}

/* ---------------- 平台推荐 ---------------- */

function highlightPlatform() {
  const platform = detectPlatform();
  if (!platform) return;
  const card = document.querySelector(`[data-platform="${platform}"]`);
  if (card) {
    card.classList.add("recommended");
    const badge = document.createElement("span");
    badge.className = "tag";
    badge.textContent = "适合当前设备";
    card.prepend(badge);
  }
}

/* ---------------- 更新日志渲染 ---------------- */

const escapeHtml = (s) =>
  s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** 极简 markdown：标题、无序列表、加粗、行内代码、链接、分隔线 */
function miniMarkdown(src) {
  const lines = src.split("\n");
  const out = [];
  let inList = false;
  const closeList = () => {
    if (inList) {
      out.push("</ul>");
      inList = false;
    }
  };
  const inline = (text) =>
    escapeHtml(text)
      .replace(/`([^`]+)`/g, "<code>$1</code>")
      .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
      .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, '<a href="$2">$1</a>');

  for (const raw of lines) {
    const line = raw.trimEnd();
    if (!line.trim()) {
      closeList();
      continue;
    }
    if (/^-{3,}$/.test(line.trim())) {
      closeList();
      continue;
    }
    const item = line.match(/^\s*[-*]\s+(.*)$/);
    if (item) {
      if (!inList) {
        out.push("<ul>");
        inList = true;
      }
      out.push(`<li>${inline(item[1])}</li>`);
      continue;
    }
    closeList();
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      out.push(`<p><strong>${inline(heading[2])}</strong></p>`);
      continue;
    }
    out.push(`<p>${inline(line)}</p>`);
  }
  closeList();
  return out.join("\n");
}

function formatDate(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

function renderChangelog() {
  const mount = $("#changelog");
  if (!mount) return;
  if (!releases.length) {
    mount.innerHTML =
      '<p class="lead-inline">暂时读不到更新日志，请直接查看 <a href="' +
      REPO +
      '/releases">GitHub Releases</a>。</p>';
    return;
  }
  mount.innerHTML = releases
    .map((rel, i) => {
      const tag = escapeHtml(rel.tag || "");
      const date = formatDate(rel.date);
      // Release 正文里 `---` 之后是 CI 自动追加的构件清单，官网上不需要重复
      const raw = (rel.body || "").split(/\n---\n/)[0].trim();
      const body = miniMarkdown(raw || "（本条发布没有附更新说明）");
      return `<article class="release">
        <h3>${escapeHtml(rel.name || tag)}
          ${i === 0 ? '<span class="latest">最新</span>' : ""}
          <span class="tag">${tag}${date ? " · " + date : ""}</span>
        </h3>
        <div class="body">${body}</div>
      </article>`;
    })
    .join("");
}

/* ---------------- 主题切换 ---------------- */

// 三种状态各一个图标：半填充圆 = 跟随系统，太阳 = 浅色，弯月 = 深色。
const THEME_ICONS = {
  auto:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true" focusable="false">' +
    '<circle cx="12" cy="12" r="9"/><path d="M12 3a9 9 0 0 1 0 18z" fill="currentColor" stroke="none"/></svg>',
  light:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" aria-hidden="true" focusable="false">' +
    '<circle cx="12" cy="12" r="4.2"/>' +
    '<path d="M12 2.6v2.3M12 19.1v2.3M2.6 12h2.3M19.1 12h2.3M5.3 5.3l1.6 1.6M17.1 17.1l1.6 1.6M18.7 5.3l-1.6 1.6M6.9 17.1l-1.6 1.6"/></svg>',
  dark:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" aria-hidden="true" focusable="false">' +
    '<path d="M20.5 14.6A8.8 8.8 0 0 1 9.4 3.5a8.8 8.8 0 1 0 11.1 11.1z"/></svg>',
};

/** 把主题按钮挂到导航栏（样式见 style.css 的 .theme-toggle） */
function mountThemeToggle() {
  const bar = $("header .nav");
  const theme = window.SodamTheme;
  if (!bar || !theme) return;

  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "theme-toggle";
  btn.dataset.themeToggle = "";
  btn.innerHTML =
    '<span class="theme-toggle-icon"></span><span class="theme-toggle-label"></span>';

  const icon = $(".theme-toggle-icon", btn);
  const label = $(".theme-toggle-label", btn);

  const render = (mode) => {
    icon.innerHTML = THEME_ICONS[mode] || THEME_ICONS.auto;
    label.textContent = theme.LABELS[mode] || "";
    // 悬停提示写「当前状态 + 点击后是什么」，比只写“切换主题”更能预期
    const next = theme.MODES[(theme.MODES.indexOf(mode) + 1) % theme.MODES.length];
    btn.title = `${theme.TITLES[mode]}（点击切换为${theme.LABELS[next]}）`;
    btn.setAttribute("aria-label", `${theme.TITLES[mode]}，点击切换主题`);
  };

  render(theme.current());
  theme.onChange(render);
  btn.addEventListener("click", () => theme.cycle());
  bar.appendChild(btn);
}

/* ---------------- 启动 ---------------- */

(async function boot() {
  await loadReleases();
  fillVersion();
  await fillDownloadLinks();
  highlightPlatform();
  renderChangelog();
  mountThemeToggle();
  wireCopyButtons();
})();
