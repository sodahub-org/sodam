<div align="center">
  <img src="crates/sodam/assets/brand/sodam-logo.svg" width="96" alt="SodaM logo">
  <h1>SodaM</h1>
  <p>基于 GPUI + Rust 的原生汽水音乐桌面客户端</p>
  <p>
    <a href="https://github.com/sodahub-org/sodam/blob/main/LICENSE"><img src="https://img.shields.io/badge/License-AGPL%203.0%20or%20later-blue.svg" alt="License: AGPL-3.0-or-later"></a>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-1.85%2B-orange.svg" alt="Rust 1.85+"></a>
    <a href="https://github.com/sodahub-org/gpui"><img src="https://img.shields.io/badge/UI-GPUI%200.2.2-8B5CF6.svg" alt="GPUI 0.2.2"></a>
    <a href="https://omarchy.org/"><img src="https://img.shields.io/badge/Linux%20%7C%20Omarchy-tested-success.svg" alt="Linux / Omarchy tested"></a>
    <img src="https://img.shields.io/badge/macOS%20%7C%20Windows-in%20development-yellow.svg" alt="macOS / Windows in development">
  </p>
  <p>音乐能力由 <a href="https://github.com/sodahub-org/libresoda">libresoda</a> 提供；应用签名服务可对接
    <a href="https://github.com/sodahub-org/libmssdk">libmssdk</a>。</p>
</div>

SodaM 面向 Linux 桌面，重点做四件事：**接近官方客户端的操作手感、稳定的本地播放体验、
会员账号的无损音质播放，以及干净自适应的主题系统**。

### 维护者与贡献者

<table align="center">
  <tr>
    <td align="center">
      <a href="https://github.com/zephyr-cheung">
        <img src="https://avatars.githubusercontent.com/u/221658147?v=4" width="72" alt="ZephyrCheung"><br />
        <strong>ZephyrCheung</strong><br />
        Maintainer
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/aBER0724">
        <img src="https://avatars.githubusercontent.com/u/69146982?v=4" width="72" alt="aBER0724"><br />
        <strong>aBER0724</strong><br />
        Contributor
      </a>
    </td>
  </tr>
</table>

### 平台支持

| 平台 | 状态 |
| --- | --- |
| Linux / Omarchy | 已实测 |
| macOS | 开发中 |
| Windows | 开发中 |

## 界面预览

| 推荐流 | 听歌模式 |
| --- | --- |
| <a href="docs/img/Discover.png"><img src="docs/img/Discover.png" width="880" alt="SodaM 推荐流页面"></a> | <a href="docs/img/Modes.png"><img src="docs/img/Modes.png" width="880" alt="SodaM 听歌模式页面"></a> |
| **搜索** | **我的歌单** |
| <a href="docs/img/Search.png"><img src="docs/img/Search.png" width="880" alt="SodaM 搜索页面"></a> | <a href="docs/img/Playlists.png"><img src="docs/img/Playlists.png" width="880" alt="SodaM 我的歌单页面"></a> |
| **我喜欢的音乐** | **设置** |
| <a href="docs/img/Liked.png"><img src="docs/img/Liked.png" width="880" alt="SodaM 我喜欢的音乐页面"></a> | <a href="docs/img/Settings.png"><img src="docs/img/Settings.png" width="880" alt="SodaM 设置页面"></a> |

## 功能特性

### 播放与队列

- 本地播放引擎：播放 / 暂停、上一首 / 下一首、拖动进度、音量控制
- 顺序、随机、单曲循环等队列模式
- 正在播放曲目在歌单列表中保持高亮
- 播放队列分为「已播放 / 正在播放 / 接下来」
- 队列与歌单中的歌曲支持右键菜单、下一首播放、收藏切换
- 歌名可跳转专辑页，歌手名可跳转音乐人页

### 播放页与歌词

- 官方式播放页布局：封面宽度 `min(40vh, 30vw)`，歌词列 `300–500px`
- 按当前歌曲封面提取主色，全局背景、表面色、强调色动态渐变
- 歌词随播放进度滚动，当前句居中高亮
- 播放页可在歌词视图与队列视图之间切换
- 封面与歌词加载失败时提供重试，并校验资源与当前歌曲是否匹配

### 推荐、听歌模式与音乐库

- 首页推荐队列，启动后自动准备第一首歌曲
- 45+ 听歌模式入口，图标本地内置，浅色 / 深色主题自动适配
- 探索歌单虚拟网格与分页加载
- 我喜欢的音乐、我的歌单、歌单详情、歌手页、专辑页
- 收藏状态在播放页、播放栏、队列和列表间保持同步

### 搜索

- 综合搜索与歌曲、音乐人、专辑、歌单垂直搜索
- 搜索结果支持多类型分组与详情页跳转
- 艺人页包含头像、热门歌曲、专辑与加载更多
- 专辑页展示封面、发行信息与全碟歌曲
- 输入框支持中文输入法与光标 / 选区操作

### 桌面体验

- Dark / Light 双主题，首次启动跟随系统偏好
- 中文 / English 界面语言，首次启动跟随系统语言
- 系统托盘：播放控制、显示主窗口、真正退出
- 关闭主窗口不退出进程，保留后台播放
- 原生窗口与桌面入口，Arch Linux ARM64 包可直接安装

### 性能与稳定性

- 网络请求、下载、解密、封面处理均放到后台线程，不阻塞渲染
- 长列表使用虚拟滚动，只渲染可见行
- 封面请求池有并发上限、去重和队列上限
- 播放请求带序号防竞态，过期结果不会覆盖当前歌曲
- 帧耗时与滚动压测开关可用于定位 UI 卡顿

## 架构与依赖

```text
SodaM
└── sodam-core        配置、会话、队列、播放引擎、音乐库封装；不依赖 GPUI
    └── libresoda     汽水接口、取流、解密、扫码登录、签名提供者
        └── libmssdk  可选应用签名服务，完整音质链路使用
```

分层细节见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)，
UI 规范见 [`docs/UI-SPEC.md`](docs/UI-SPEC.md)。

## 快速开始

### 环境要求

- Rust stable，最低 `1.85`
- Linux Wayland 或 X11 运行时
- Chromium / Chrome / Edge 等浏览器用于扫码登录签名页
- `libresoda` 与 `libmssdk` 建议与 `sodam` 同级 clone

```text
Projects/sodahub-org/
├── libresoda/
├── libmssdk/
└── sodam/
```

### 启动

```bash
cd sodam
scripts/run.sh
```

首次启动进入设置页，扫码登录后配置会写入本地。已登录用户会直接进入播放页，
并自动准备推荐队列。

## 配置

配置文件：

```text
~/.config/sodam/config.json
```

环境变量只在配置字段为空时补全。常用配置：

| 变量 | 说明 |
| --- | --- |
| `SODA_COOKIE` | 登录 Cookie，扫码成功后自动写入配置 |
| `QISHUI_SIGNER_URL` | 签名服务 `/sign` 地址 |
| `QISHUI_SIGNER_TOKEN` | 签名服务 Bearer Token |
| `SODA_DEVICE_ID` / `SODA_IID` / `SODA_FP` | 设备身份；覆盖时必须与签名器一致 |
| `SODAM_QUALITY` | `best` / `lossless` / `highest` / `medium` / `low` |

SodaM 默认内置一个公共签名服务，未部署 `libmssdk` 的用户也可以直接使用。
可以在设置页或环境变量中替换为自己的签名服务。

不要提交真实 Cookie、Token、签名头或设备指纹。

## 开发

本仓库为了避免 ARM64 开发机编译峰值导致 OOM，所有常规操作都使用带资源护栏的脚本：

```bash
scripts/check.sh   # cargo check + fmt + clippy + test
scripts/build.sh   # debug / release 构建
scripts/run.sh     # 启动 GUI，带 MemoryMax 护栏
```

不要在 GUI 运行时编译；不要绕过脚本反复执行全量 cargo 构建。

## 打包

```bash
cd packaging/arch
PKGEXT='.pkg.tar.zst' makepkg -f
sudo pacman -U ./sodam-<version>-<arch>.pkg.tar.zst
```

安装后会得到：

```text
/usr/bin/sodam
/usr/share/applications/sodam.desktop
/usr/share/icons/hicolor/256x256/apps/sodam.png
```

## 文档

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)：分层、线程、播放与导航架构
- [`docs/UI-SPEC.md`](docs/UI-SPEC.md)：主题、布局、尺寸与性能约束
- [`docs/ROADMAP.md`](docs/ROADMAP.md)：已完成能力与后续优先级

## 许可与合规

代码使用 AGPL-3.0-or-later。

SodaM 是独立项目，与字节跳动、抖音或汽水音乐无关。它只面向个人播放自己有权访问的
内容；不得用于公共代理、批量抓取、绕过付费限制或分发凭据。
