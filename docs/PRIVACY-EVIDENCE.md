# SodaM 数据流与隐私证据

本文记录 SodaM 数据流对应的代码位置，供隐私审计和 WinGet 审核复核。
它不是法律意见，也不能替代第三方网络、主机或反向代理的审计。

## 结论范围

准确的说法是：

- SodaM **不运营广告或用户画像服务**；必要的播放诊断和兼容事件会直接发往汽水；
- SodaM 会把用户主动发起的汽水 API 请求直接发往 `https://api.qishui.com`；
- 需要官方原生签名头的请求会把真实请求头（其中可能含 Cookie）发送给签名服务，
  这是汽水请求签名契约决定的；
- 默认签名服务的应用业务代码和应用日志不记录 Cookie、Token 或签名头，
  但宿主机、容器运行时、反向代理、交换分区和有运维权限的人员不在该保证范围内。

因此，不能声称“默认签名服务从技术上永远看不到 Cookie”。正确说法是
“签名服务为完成签名会处理 Cookie，但业务日志不记录、业务代码不持久化它”。

## SodaM 客户端数据流

### 汽水 API 直接请求

- `libresoda/src/soda/mod.rs` 的 `pc_post_json` 直接请求
  `https://api.qishui.com{path}`。
- `pc_request_options` 把账号 Cookie附加到发给汽水 API 的请求。
- 播放、搜索、推荐、歌单、歌词和账号信息不经过 SodaM 自有后端。

核验命令：

```bash
rg -n "api\\.qishui\\.com|pc_post_json|pc_request_options" libresoda/src/soda
```

### 签名请求

- `libresoda/src/soda/signature.rs` 的 `HttpSignature::sign` 将
  `SignRequest { url, method, body, headers }` 作为 JSON POST 到签名地址。
- `apply_stream_signature` 先合并真实待发请求头，再交签名服务；
  这是因为汽水签名覆盖 URL 和请求头，不能只发一个空头集合。

核验命令：

```bash
rg -n "SignRequest|serde_json::to_vec|post_json|head" libresoda/src/soda/signature.rs
```

### 本地保存

- `sodam/crates/sodam-core/src/config.rs` 定义配置字段和原子写盘；
- SodaM 客户端配置可能在 Windows `%APPDATA%\\sodam\\config.json`、
  Linux `~/.config/sodam/config.json`、
  macOS `~/Library/Application Support/sodam/config.json`；
- 配置中的 Cookie、签名 Token、设备标识和搜索历史只保存在本地，
  除非用户把该文件自行上传或导出；
- 音频与封面缓存只写入本地缓存目录。
- Unix 单实例 socket 只用于激活已有窗口，见
  `sodam/crates/sodam/src/single_instance.rs`；不含账号或播放记录。

核验命令：

```bash
rg -n "save_to_locked|config_path|cache_dir|search_history" \\
  sodam/crates/sodam-core/src
```

## 默认签名服务代码

本节的路径以 `libmssdk` 仓库根目录为准。

### 请求如何被读取

- `src/server.mjs` 的 `readBody` 读取签名请求 JSON；
- 只有 `POST /sign` 会调用 `normalizeSignRequest`，
  再把 headers 交给 `signer.sign`；
- `/probe` 与 `/track/stream` 也能接收 Cookie 并直接请求汽水 API。
  SodaM 正常播放走 `/sign`；公网部署应按 `deploy/README.md` 在反向代理
  屏蔽这两个诊断路由；
- Cookie 是 headers 的一个字段，因此确实进入签名进程内存。

### 业务日志实际记录什么

`POST /sign` 的 `logEvent('sign', {...})` 只记录：

- 请求路径（不含 query）；
- HTTP method；
- headers 的数量；
- 请求体字节数；
- 脱敏后的设备号；
- 耗时。

`src/log.mjs` 的 secret 匹配列表覆盖
`cookie`、`authorization`、`set-cookie`、`x-helios`、`x-medusa`、
`x-ss-stub`。`redactHeaders` 在这些头被记录时只输出长度。
当前 `/sign` 的日志路径甚至不遍历 header value。

此外，`libmssdk` 应用默认没有启用 HTTP access log，
也没有把请求内容写入持久卷。持久卷只保存设备身份 `identity.json`
和 Wine prefix；前者不含 Cookie。该结论只覆盖应用层，不能覆盖宿主机
交换分区、内存转储、容器平台日志或运维权限。

### 运行时核对

启动默认签名服务后，发送一次带假 Cookie 的签名请求，再检查：

```bash
rg -n "cookie|Cookie|sessionid|authorization|Authorization" \\
  libmssdk/data libmssdk/logs 2>/dev/null || true
```

如果日志被外部容器采集器或反向代理默认记录，这属于部署配置，
不是上述应用代码保证的一部分。只允许 `/sign` 暴露、使用 HTTPS、
限制来源并关闭代理的请求头/请求体日志。对安全要求高的部署还应启用
交换分区加密和主机访问控制。

默认签名地址当前是明文 HTTP（`sodam/crates/sodam-core/src/config.rs`），
所以隐私政策必须披露传输风险，不能写成所有签名流量都已加密。

## WinGet 审核建议的表述

不要写“没有收集 Cookie”。建议写：

> SodaM keeps the login session locally. Requests to Qishui Music are sent
> directly to Qishui Music's official services. Because Qishui's request
> signature covers request headers, the signing request may include the login
> Cookie. The default signing service processes it only to complete the
> signature. Its application code and application logs do not persist the
> Cookie, signing tokens, or generated signature headers. Host, container,
> reverse-proxy, and operational access are outside that application-level
> guarantee. The built-in signer URL currently uses HTTP; users who need
> encrypted transport should configure an HTTPS signer or run libmssdk locally.

并在 `PRIVACY.md` 中保留相同结论。这个表述与实际代码一致，
比“我们绝无可能看到 Cookie”更可信。
