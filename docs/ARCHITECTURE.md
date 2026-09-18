# 架构

## 总览

```text
┌────────────────────────── crates/sodam（GPUI 层） ───────────────────────────┐
│ Root                                                                          │
│  ├── 导航 / 设置 / 搜索输入 / 播放队列缓存 / 封面缓存 / ambient colors        │
│  ├── app_actions.rs    播放、队列、收藏、歌单、推荐、设置、封面动作             │
│  ├── app_search.rs     搜索、艺人页、专辑页动作                                │
│  ├── ui::sidebar       侧边栏、品牌、导航                                     │
│  ├── views::*          Discover / Scenes / Search / Artist / Album / ...      │
│  ├── ui::player_bar    播放控制、进度、音量、播放队列                          │
│  ├── ui::theme         Dark/Light、ambient color、尺寸、共享颜色               │
│  └── tray.rs           StatusNotifierItem 托盘与托盘菜单                      │
└───────────────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌──────────────────── crates/sodam-core（领域层，无 GPUI） ─────────────────────┐
│ Settings       配置文件 + 环境变量补全                                        │
│ Session        Cookie、AppCredentials、签名 provider、音质偏好                │
│ library        推荐 / 听歌模式 / 搜索 / 歌单 / 收藏 / 艺人 / 专辑 / 歌词       │
│ Queue          当前曲目、历史、下一首、随机 / 顺序 / 单曲循环                  │
│ PlaybackEngine rodio 线程；接收 Load / Play / Pause / Seek / Volume 命令       │
│ AudioColor     封面主色提取                                                   │
└───────────────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
                       libresoda（汽水接口、取流、解密、签名）
                                 │
                       libmssdk（可选远程签名服务）
```

## 状态模型

`Root` 是 GPUI 侧唯一根实体。子模块不保存第二份全局状态，只读 `&Root` 或通过
`cx.listener` / entity update 派发动作。列表滚动句柄、菜单位置、搜索输入等 UI 状态
放在 `Root`；缓存数据用 `Arc` 共享，避免渲染路径深拷贝。

当前曲目的取值顺序是：

```text
pending_track（正在装载的新歌）
  → queue.current()（已生效的当前歌）
```

这样可以避免切歌时出现“界面显示 A、耳朵听到 B”的竞态。

## 线程与异步

libresoda 的 HTTP 和下载是阻塞的；GPUI 渲染不能阻塞。规则如下：

1. 网络请求、文件下载、解密、封面缩放放到 `cx.background_spawn` 或专用线程。
2. 完成后用 `cx.spawn` + entity update 回到 GPUI 实体上下文，然后 `cx.notify()`。
3. 带分页 / 可取消语义的请求使用 `request_seq` 丢弃过期结果。
4. 封面通过 `CoverPool` 有界并发请求，渲染路径只查本地缓存。
5. 音频线程独立于 GPUI，通过 channel 接收命令，通过 snapshot 共享状态。

## 播放链路

```text
TrackItem + cache path
  → PlaybackEngine::load(track, path, quality)
  → rodio Decoder + Sink
  → snapshot: track_id / title / playing / position / duration / quality / error
```

播放结束时音频线程标记 `finished`；GPUI 心跳读取 snapshot，调用 `next_track`。
签名不足或设备不匹配时，libresoda 可能只返回试听；错误会进入状态行或播放页重试区。

## 导航与详情页

`Nav` 包含固定入口和详情页：`Discover`、`Listening Modes`、`Search`、`Liked Music`、
`My Playlists`、`Artist`、`Album`、`Settings`、`Now Playing`。

- 详情页使用 `nav_history` 做返回。
- 歌手页 / 专辑页由 `app_search.rs` 拉取并缓存在 `open_artist` / `open_album`。
- 曲目里的歌名和歌手可以打开专辑 / 音乐人页；点击事件会 stop propagation，
  避免触发行点击播放。

## 队列与历史

`Queue` 保存 upcoming 列表和当前索引；播放后的曲目进入 `played_history`。
队列展示分为已播放、正在播放、接下来三段。随机播放会重建 upcoming 顺序；
单曲循环在自然结束时重复当前曲。

## 主题、ambient color 与 i18n

- 主题支持 `Dark`、`Light` 和首次启动跟随系统。
- 封面主色提取后写入 `ambient_colors`，再由 `theme::set_ambient_rgb` 影响
  背景、表面、边框、强调色。
- 左上角与设置页的四色 logo 基于当前封面主色生成：主色、互补色、两个相近色。
- 界面文案使用 `ui/i18n.rs`；中文原文是稳定 key，英文由词典映射。

## 打包与运行时

- Linux ARM64 使用 `packaging/arch/PKGBUILD` 生成 pacman 包。
- 安装内容：`/usr/bin/sodam`、desktop entry、hicolor icon。
- GUI 通过 `scripts/run.sh` 启动时带 `MemoryMax=2G` 护栏。
- 托盘和窗口关闭逻辑使用显式退出模式：关闭主窗口保留进程，退出走托盘菜单。
