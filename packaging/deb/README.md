# Debian / Ubuntu 包

发布用的 `.deb` 由 `make-deb.sh` 从**已构建**的 release 二进制直接打包，
不额外编译（不走 `cargo-deb`，避免第二次全量构建）。

## 构建

```bash
cargo build --release --locked --workspace
packaging/deb/make-deb.sh <x86_64|aarch64> [version] [release]
```

- 版本默认取 `Cargo.toml` 的 `[workspace.package] version`，发布号默认 `1`；
- 架构映射：`x86_64` → `amd64`，`aarch64` → `arm64`；
- 输出 `sodam_<version>_<arch>.deb` 与 `sodam_<version>_<arch>.deb.sha256`；
- 运行依赖：`dpkg-deb`（`dpkg-dev`）、`sha256sum`、`install`，只需在 Linux 上执行。

## 包内容

```text
/usr/bin/sodam
/usr/share/applications/sodam.desktop
/usr/share/icons/hicolor/256x256/apps/sodam.png
/usr/share/doc/sodam/{copyright,LICENSE,NOTICE.md}
```

desktop 入口与图标和 pacman 包共用 `packaging/arch/sodam.desktop` 与
`crates/sodam/assets/brand/sodam-logo.png`。

## 依赖对照

控制文件里的运行依赖与 `packaging/arch/PKGBUILD` 的 `depends` 一一对应，
只把 Arch 包名换成 Debian/Ubuntu 包名：

| Arch | Debian / Ubuntu |
| --- | --- |
| `alsa-lib` | `libasound2` |
| `dbus` | `libdbus-1-3` |
| `fontconfig` | `libfontconfig1` |
| `freetype2` | `libfreetype6` |
| `hicolor-icon-theme` | `hicolor-icon-theme` |
| `libxkbcommon` | `libxkbcommon0` |
| `libxcb` | `libxcb1` |

包版本是 `<upstream>-<release>`（例如 `0.3.8-1`），文件名按发布约定只带 upstream 版本。
Debian 与 Ubuntu 上都没有 GPG 签名的 apt 源，所以只提供单包下载 + `SHA256SUMS` 校验，
安装/升级/卸载都用 `apt install ./…deb` 与 `apt remove sodam`（见 README「Debian / Ubuntu 下载安装」）。

## 本机验收

在 Debian/Ubuntu 上打包后可以先不安装，直接看元数据和文件清单：

```bash
dpkg-deb --info sodam_<version>_<arch>.deb
dpkg-deb --contents sodam_<version>_<arch>.deb
sudo apt install ./sodam_<version>_<arch>.deb   # 安装后从应用菜单启动 SodaM
sudo apt remove sodam                            # 卸载，保留 ~/.config/sodam
```

CI 的 Linux 矩阵会构建两个架构的 `.deb`，并校验 `Architecture` 与包内 `/usr/bin/sodam`。
