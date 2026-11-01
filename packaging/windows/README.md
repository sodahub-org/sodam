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
- **目标 `sodam.exe` 正在运行时，静默安装会直接中止**（退出码 7），且不会弹窗。先退出应用再装。
- 静默安装带上 `/LOG=`，否则中止原因不可见：

```powershell
Start-Process .\sodam-<版本>-windows-x64-setup.exe -Wait -PassThru `
  -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',"/LOG=$env:TEMP\sodam-setup.log"
```

## 本机验收

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
