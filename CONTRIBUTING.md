# 参与 SodaM 开发

SodaM 是**汽水音乐的第三方桌面客户端**（与官方无关），基于 GPUI + Rust。
欢迎 issue、代码、文档和平台适配。动手前先读这份文档，能省掉来回沟通。

- 官网 / 安装说明：<https://sodahub-org.github.io/sodam/>
- 开发者文档：<https://sodahub-org.github.io/sodam/developers.html>
- 反馈问题：用 [issue 模板](https://github.com/sodahub-org/sodam/issues/new/choose)

## 环境要求

| 项目 | 要求 |
| --- | --- |
| Rust | 1.85+（workspace `rust-version`） |
| Linux 依赖 | `alsa-lib` `dbus` `fontconfig` `freetype2` `hicolor-icon-theme` `libxkbcommon` `libxcb` |
| macOS | Apple Silicon，Xcode Command Line Tools |
| Windows | x64，MSVC 工具链 |

## 构建与运行

```bash
git clone https://github.com/sodahub-org/sodam
cd sodam

scripts/check.sh   # cargo check + fmt --check + clippy -D warnings + test
scripts/build.sh   # 构建（--release 出正式包）
scripts/run.sh     # 启动 GUI（带 MemoryMax 护栏）
```

**资源护栏**（这台开发机 15GB 内存、曾因 GPUI 编译触发 OOM）：

- 一律用上面三个脚本，不要裸跑 `cargo build` 做全量 release；
- 编译时不跑 GUI，跑 GUI 时不编译；
- 卡顿时先看 `journalctl -k | grep -iE "drm|flip_done|oom"`。

## 代码结构

| 路径 | 职责 |
| --- | --- |
| `crates/sodam-core/` | 领域层：配置、会话、队列、音乐库、播放引擎（**不依赖 GPUI**） |
| `crates/sodam/` | GPUI 应用：入口、Root 状态、动作、托盘、`ui/`、`views/` |
| `packaging/arch/` | PKGBUILD、desktop 文件、`mkrepo.sh`（生成 pacman 源） |
| `packaging/homebrew/` | Homebrew Cask |
| `website/` | 官网静态页面（CI 部署到 GitHub Pages） |
| `docs/` | ARCHITECTURE / ROADMAP / UI-SPEC |

## 开发约定

1. **领域层不依赖 GPUI**：`sodam-core` 保持纯 Rust，可单独测试。
2. **不阻塞渲染线程**：libresoda 的 HTTP/下载是阻塞操作，必须放在 `cx.background_spawn`
   或专用线程，结果回 UI 后 `cx.notify()`。
3. **状态只有一处**：播放、队列、导航、设置都由 `Root` 或 core 模型持有；UI 只读状态、
   生成元素、派发动作。
4. **错误不 panic**：网络/配置错误写进状态行（`Root::status_line`，见 `set_status_text`）
   或页面提示；只有启动期不可恢复错误才允许 panic。
5. **凭据不入库、不入日志**：Cookie、签名 Token、抓包签名头、真实设备指纹禁止提交或打印。
6. **界面文案走 i18n**：新增用户可见文案用 `root.tr` / `root.localized`，中文原文作为
   稳定 key，并同步英文词典。
7. **接口能力先在 libresoda 打通**：SodaM 只做调用与展示，不绕过 libresoda 直接请求汽水接口。
8. **UI 改动必须真机验证**：用 `scripts/run.sh` 启动确认，不能只过 `cargo check`。
9. **提交前必须跑** `scripts/check.sh`。

## 提交 Pull Request

1. 从 `main` 切分支：`feat/…`、`fix/…`、`docs/…`、`chore/…`。
2. commit message 用类型前缀 + 中文祈使句，例如：
   ```
   fix(media): 修 Linux 托盘「未在播放」与 MPRIS 注册失败不重试
   feat(ui): 播放栏加入播放模式切换
   ```
   正文写清「问题 / 原因 / 修复 / 验证」，比一句「fix bug」有用得多。
3. PR 描述按模板填写（问题、原因、修复、验证、检查清单）。
4. CI 会跑四平台：Linux x86_64 / aarch64、macOS arm64、Windows x64。
5. Review 通过后 squash 合并。

**小改动也要有验证**：能附一条复现命令、一段日志或一张真机截图，review 会快很多。

## 发布流程（维护者）

1. 改版本号：`Cargo.toml` 的 `[workspace.package] version`（`Cargo.lock` 同步）、
   `packaging/arch/PKGBUILD`、README 里的版本示例。
2. 发版工作流以 `workflow_dispatch` 先构建产物；从 `SHA256SUMS` 取 macOS zip
   的 sha256，更新 `packaging/homebrew/sodam.rb` 后提交。
3. 在包含正式 cask 校验和的提交上打**附注 tag**，tag message 就是发布说明：
   ```bash
   git tag -a v0.3.2 -F changelog.txt
   git push origin main && git push origin v0.3.2
   ```
4. tag 触发的 CI 构建四平台产物、创建 GitHub Release，并自动重建 pacman 源与官网。
5. 用 `packaging/homebrew/sync-tap.sh` 把同一 cask 同步到
   [homebrew-tap](https://github.com/sodahub-org/homebrew-tap)。

## 行为准则

- 对事不对人，技术分歧用数据说话（日志、复现、基准）。
- 不讨论、不索取任何账号凭据；不发布绕过官方限制的教程。
- 提 issue 前先搜一下，重复的会被合并。
