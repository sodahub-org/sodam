// 站点主题：默认跟随设备外观（prefers-color-scheme），手动切换后写进 localStorage。
//
// 这个文件必须放在 <head> 里**同步**加载（不要加 defer / async）：它要在首次绘制之前
// 把 data-theme / data-theme-mode 写到 <html> 上，否则浅色系统下会先闪一帧深色。
//
// 切换按钮由 assets/app.js 负责挂到导航栏，这里只暴露 window.SodamTheme。
// 关闭 JS 时页面保持深色（站点其它功能同样依赖 app.js，不再单独做 no-JS 兜底）。
(function () {
  "use strict";

  var KEY = "sodam:theme";
  var MODES = ["auto", "light", "dark"];
  var LABELS = { auto: "跟随系统", light: "浅色", dark: "深色" };
  var TITLES = {
    auto: "主题：跟随系统外观",
    light: "主题：浅色",
    dark: "主题：深色",
  };
  // 与 style.css 里的 --bg 保持一致：浏览器地址栏 / PWA 标题栏取这个值
  var COLORS = { light: "#f6f7f7", dark: "#0d0f10" };

  var root = document.documentElement;
  var media = window.matchMedia ? window.matchMedia("(prefers-color-scheme: light)") : null;
  var listeners = [];

  function readMode() {
    try {
      var value = localStorage.getItem(KEY);
      return MODES.indexOf(value) >= 0 ? value : "auto";
    } catch (err) {
      // 隐私模式 / 存储被禁用：退化成「跟随系统」，不抛错
      return "auto";
    }
  }

  function resolve(mode) {
    if (mode !== "auto") return mode;
    return media && media.matches ? "light" : "dark";
  }

  function paintMeta(theme) {
    var metas = document.querySelectorAll('meta[name="theme-color"]');
    for (var i = 0; i < metas.length; i++) {
      metas[i].setAttribute("content", COLORS[theme]);
    }
  }

  function apply(mode, persist) {
    var theme = resolve(mode);
    root.setAttribute("data-theme", theme);
    root.setAttribute("data-theme-mode", mode);
    paintMeta(theme);
    if (persist) {
      try {
        localStorage.setItem(KEY, mode);
      } catch (err) {
        /* 存储不可用就只在本次会话生效 */
      }
    }
    for (var i = 0; i < listeners.length; i++) listeners[i](mode);
  }

  function cycle() {
    var next = MODES[(MODES.indexOf(readMode()) + 1) % MODES.length];
    apply(next, true);
    return next;
  }

  apply(readMode(), false);

  // 「跟随系统」时系统外观一变就立刻跟上：CSS 变量由 data-theme 决定，
  // 这里只把 meta theme-color 和按钮文案同步过去。
  if (media) {
    var onSchemeChange = function () {
      if (readMode() === "auto") apply("auto", false);
    };
    if (media.addEventListener) media.addEventListener("change", onSchemeChange);
    else if (media.addListener) media.addListener(onSchemeChange);
  }

  window.SodamTheme = {
    MODES: MODES,
    LABELS: LABELS,
    TITLES: TITLES,
    current: readMode,
    effective: function () {
      return resolve(readMode());
    },
    apply: function (mode) {
      if (MODES.indexOf(mode) >= 0) apply(mode, true);
    },
    cycle: cycle,
    onChange: function (fn) {
      listeners.push(fn);
      return function () {
        var at = listeners.indexOf(fn);
        if (at >= 0) listeners.splice(at, 1);
      };
    },
  };
})();
