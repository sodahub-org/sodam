# Windows 安装包与本机验收

## 构建

CI 的 Windows 安装包 job 用 `packaging/windows/sodam.iss` 生成
`sodam-<版本>-windows-x64-setup.exe`。验证通道的版本串带 `-test.<运行号>`，不会顶掉同版本号的正式包。

本地手动构建需要 Inno Setup 6：

```powershell
ISCC.exe /DMyAppVersion=<版本> packaging/windows/sodam.iss
```

## 安装行为

- 按用户安装（HKCU，无需管理员）；首次安装默认到 `%LOCALAPPDATA%\Programs\SodaM`。
- **升级会沿用上次的安装目录**：`DefaultDirName` 只对首次安装生效，别假设新包一定装到默认路径。
  安装后可在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{A1AAB6BA-...}_is1`
  的 `InstallLocation` 与 `DisplayVersion` 确认目标目录和实际版本。
- **安装时会自动结束安装目录下的 `sodam.exe`**（issue #184）：在开始拷文件前按完整路径精确
  匹配并强制结束，按墙钟等待约 5 秒确认进程退出、文件解锁；不弹「准备安装」页，也没有重启
  管理器的 30 秒等待。
  交互和静默安装行为一致，中途取消安装不会杀进程。
- **只杀安装目录下的那个实例**：按 `ExecutablePath` 全路径比对，装在别处的 SodaM 不受影响。
  杀不掉时才会中止安装（比如另一个登录会话里还开着 SodaM），此时不改动任何文件，报错提示
  从托盘退出后重试。
- 静默安装带上 `/LOG=`，便于排查中止原因：

```powershell
Start-Process .\sodam-<版本>-windows-x64-setup.exe -Wait -PassThru `
  -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=$env:TEMP\sodam-setup.log"
```

## 本机验收

安装器改动（#184）需真机过四种情形：

1. 主窗口在前台时直接安装：不弹「准备安装」页，一次装完，无 30 秒等待。
2. 只留托盘时安装：同上。
3. 静默安装（`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART`）：退出码 0。
4. 开始安装前点取消：用户的 SodaM 不被杀掉。

- 日志目录 `%LOCALAPPDATA%\sodam\logs` 与缓存目录 `%LOCALAPPDATA%\sodam\audio`、
  `%LOCALAPPDATA%\sodam\covers` 是兄弟目录；「清空缓存」不应删除日志。
- 多个构建或实例共用同一个用户数据目录。验收前先退出旧实例，避免旧进程继续写日志或占用文件。
- 日志敏感字段粗查：

```powershell
Select-String -Path "$env:LOCALAPPDATA\sodam\logs\sodam.log" `
  -Pattern 'cookie|authorization|x-helios|x-medusa|token' -CaseSensitive:$false
```

- 构造「删除日志失败」时，用不共享 `DELETE` 的句柄锁住 `sodam.log`：
  `[System.IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')`。
  只读属性挡不住当前 Rust 的 `remove_file`（Windows 上会退化为 POSIX delete）。
