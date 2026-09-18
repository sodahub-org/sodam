# AGENTS.md — sodam 开发约定

面向在本仓库工作的 AI agent / 协作者。动代码前先读本文件。

## 项目定位

`sodam`（SodaM）是基于 **GPUI** 的汽水音乐桌面客户端。音乐能力来自
[libresoda](https://github.com/sodahub-org/libresoda)（接口、取流、解密），应用签名来自
[libmssdk](https://github.com/sodahub-org/libmssdk)（可选远程 HTTP 签名服务）。

风格参考汽水音乐官方 PC 客户端和 [sonora](https://github.com/sonorahq/sonora)。
只借鉴工程思路与交互；不复制 GPL 代码。

## 目录

```text
crates/sodam-core/       领域层：配置 / 会话 / 队列 / 音乐库 / 播放引擎（不依赖 GPUI）
  config.rs              配置读写 + 环境变量补全
  session.rs             配置 → libresoda Soda（Cookie、签名器、音质）
  library.rs             推荐、听歌模式、搜索、歌单、收藏、艺人/专辑数据
  models.rs              TrackItem / PlaylistItem / ArtistItem / AlbumItem / SceneItem
  queue.rs               播放队列与播放模式
  audio.rs               rodio 播放线程
crates/sodam/            GPUI 应用
  main.rs                入口、窗口、托盘启动
  app.rs                 Root 状态、输入法、渲染入口
  app_actions.rs         播放、队列、收藏、歌单、推荐、封面、设置动作
  app_search.rs          搜索、艺人页、专辑页动作
  tray.rs                StatusNotifier 托盘
  ui/                    theme、sidebar、player_bar、artwork、icons、i18n
  views/                 Discover、Scenes、Search、Artist、Album、Library、Lyrics、Settings
docs/ARCHITECTURE.md     架构与线程模型
docs/ROADMAP.md          当前状态与剩余工作
docs/UI-SPEC.md          UI 约束与主题规范
packaging/arch/          Arch Linux PKGBUILD 与 desktop 文件
```

## 硬性约定

1. **领域层不依赖 GPUI**：`sodam-core` 保持纯 Rust，可单独测试。
2. **不阻塞渲染线程**：libresoda 的 HTTP/下载是阻塞操作；网络与文件处理必须放在
   `cx.background_spawn` 或专用线程，结果回 UI 后 `cx.notify()`。
3. **状态只有一处**：播放、队列、导航、设置都由 `Root` 或 `sodam-core` 模型持有；
   UI 只读状态、生成元素、派发动作。
4. **错误不 panic**：网络/配置错误写入 `Root::status` 或页面提示；只有启动期不可恢复
   错误才允许 panic。
5. **凭据不入库、不入日志**：Cookie、签名 Token、抓包签名头、真实设备指纹禁止提交或打印。
6. **UI 改动必须真机验证**：使用 `scripts/run.sh` 启动确认，不能只过 `cargo check`。
7. **提交前必须跑** `scripts/check.sh`（check + fmt + clippy + test，并带资源护栏）。
8. **接口能力先在 libresoda 打通**：`sodam` 只做调用与展示；不要绕过 libresoda 请求汽水接口。
9. **界面文案走 i18n**：新增用户可见文案使用 `root.tr` / `root.localized`，
   中文原文作为稳定 key，并同步英文词典。
10. **不要引入个人数据**：不要提交真实 Cookie、抓包签名头、本机路径或账号数据。

## 资源护栏

2026-09-15 这台 Arch Linux ARM64 / 15GB 设备出现过编译与测试引发的 OOM。必须遵守：

1. 使用 `scripts/check.sh` / `scripts/build.sh` / `scripts/run.sh`；
2. 编译时不运行 GUI，运行 GUI 时不编译；
3. 大改后优先 `scripts/check.sh`，不要无护栏反复全量 release 编译；
4. 卡顿时先看 `journalctl -k | grep -iE "drm|flip_done|oom"`。

## 与 libresoda 的接口

| 功能 | libresoda 能力 |
| --- | --- |
| 推荐流 / 听歌模式 | feed mode、discover mix、discover、radio/scene 队列 |
| 搜索 | song/all/artist/album/playlist 搜索、suggest、suggest words |
| 我喜欢的音乐 / 收藏 | collect / uncollect、collected items/artists |
| 我的歌单 | user playlists、playlist songs、创建/加歌/删除等写操作 |
| 播放 | stream access、download info、download、解密 |
| 元数据 | lyrics、song、album detail、artist detail/tracks/albums |
| 播放记录 | recently played read/write/delete、media stats |

详见 [`libresoda CLIENT-API`](https://github.com/sodahub-org/libresoda/blob/main/docs/CLIENT-API.md)。
