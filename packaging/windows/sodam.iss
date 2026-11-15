; SodaM Windows installer script (Inno Setup 6).
; Build (from repo root):
;   ISCC.exe /DMyAppVersion=<version> packaging/windows/sodam.iss
; Output: release/sodam-<version>-windows-x64-setup.exe
; Per-user install (no admin required), default dir {localappdata}\Programs\SodaM.
; Before copying files, the installer force-closes the sodam.exe under the
; install directory (issue #184): no Restart Manager prompt, no 30s wait.
; NOTE: this file is UTF-8 with BOM: it carries Simplified Chinese messages below,
; and Inno reads a non-BOM script in the ANSI codepage.

#define MyAppName "SodaM"
#ifndef MyAppVersion
#define MyAppVersion "0.1.1"
#endif

[Setup]
AppId={{A1AAB6BA-EDE3-461D-9961-DA3CAAC0CFEC}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=sodahub-org
AppPublisherURL=https://github.com/sodahub-org/sodam
AppSupportURL=https://github.com/sodahub-org/sodam/issues
DefaultDirName={localappdata}\Programs\SodaM
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\..\release
OutputBaseFilename=sodam-{#MyAppVersion}-windows-x64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\sodam.exe
CloseApplications=no
RestartApplications=no
ShowLanguageDialog=no
UsePreviousLanguage=no

; English is listed first: it is the fallback for any system display language
; that is not Simplified Chinese (detection order is full LCID, then primary
; language, then the first entry here).
[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\sodam.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\SodaM"; Filename: "{app}\sodam.exe"
Name: "{autodesktop}\SodaM"; Filename: "{app}\sodam.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\sodam.exe"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[CustomMessages]
SodamStillRunning=Unable to close SodaM. Quit it from the tray menu (or end sodam.exe in Task Manager), then run Setup again.
chinesesimplified.SodamStillRunning=无法关闭 SodaM，安装已中止。请从托盘菜单退出 SodaM（必要时在任务管理器中结束 sodam.exe），然后重新运行安装程序。

[Code]
const
  GENERIC_WRITE = $40000000;
  OPEN_EXISTING = 3;
  ERROR_SHARING_VIOLATION = 32;
  INVALID_HANDLE_VALUE = -1;

function CreateFileW(lpFileName: String; dwDesiredAccess: Cardinal;
  dwShareMode: Cardinal; lpSecurityAttributes: Cardinal;
  dwCreationDisposition: Cardinal; dwFlagsAndAttributes: Cardinal;
  hTemplateFile: Cardinal): Integer;
  external 'CreateFileW@kernel32.dll stdcall delayload';

function CloseHandle(hObject: Integer): Boolean;
  external 'CloseHandle@kernel32.dll stdcall delayload';

function SodamExeRunning: Boolean;
var
  H: Integer;
  ExePath: String;
begin
  Result := False;
  ExePath := ExpandConstant('{app}\sodam.exe');
  if not FileExists(ExePath) then
    Exit;

  H := CreateFileW(ExePath, GENERIC_WRITE, 0, 0, OPEN_EXISTING, 0, 0);
  if H = INVALID_HANDLE_VALUE then
    Result := (DLLGetLastError = ERROR_SHARING_VIOLATION)
  else
  begin
    CloseHandle(H);
    Result := False;
  end;
end;

{ Force-close the sodam.exe under the install directory and wait (up to 5s)
  for it to go away. Matched by full ExecutablePath, not just the process
  name, so a SodaM installed elsewhere is never touched. The helper script
  polls until no matching process remains AND the exe file can be opened
  again (lock released). Returns True only when the script confirms that;
  False covers both "still running" and "the helper itself failed" (the
  caller falls back to a final CreateFileW probe to tell those apart). }
function StopSodamProcess: Boolean;
var
  ScriptPath: String;
  Script: String;
  ResultCode: Integer;
begin
  Result := False;
  ScriptPath := ExpandConstant('{tmp}\sodam-close.ps1');
  Script :=
    '$target = $args[0]' + #13#10 +
    'Get-CimInstance Win32_Process -Filter "Name=''sodam.exe''" | ' +
    'Where-Object { $_.ExecutablePath -eq $target } | ' +
    'ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }' + #13#10 +
    '$sw = [System.Diagnostics.Stopwatch]::StartNew()' + #13#10 +
    'while ($sw.ElapsedMilliseconds -lt 5000) {' + #13#10 +
    '  $alive = Get-CimInstance Win32_Process -Filter "Name=''sodam.exe''" ' +
    '-OperationTimeoutSec 3 | ' +
    'Where-Object { $_.ExecutablePath -eq $target }' + #13#10 +
    '  $locked = $false' + #13#10 +
    '  if (Test-Path -LiteralPath $target) {' + #13#10 +
    '    try { $fs = [System.IO.File]::Open($target, ''Open'', ''ReadWrite'', ''None''); $fs.Close() } catch {' + #13#10 +
    '      $inner = $_.Exception.InnerException' + #13#10 +
    '      if ($inner -is [System.UnauthorizedAccessException]) { $locked = $false }' + #13#10 +
    '      elseif ($inner -is [System.IO.FileNotFoundException]) { $locked = $false }' + #13#10 +
    '      else { $locked = $true }' + #13#10 +
    '    }' + #13#10 +
    '  }' + #13#10 +
    '  if (-not $alive -and -not $locked) { exit 0 }' + #13#10 +
    '  Start-Sleep -Milliseconds 200' + #13#10 +
    '}' + #13#10 +
    'exit 1';
  if not SaveStringToFile(ScriptPath, Script, False) then
    Exit;
  if not Exec('powershell.exe',
      '-NoProfile -ExecutionPolicy Bypass -File "' + ScriptPath + '" "' +
      ExpandConstant('{app}\sodam.exe') + '"',
      '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Exit;
  Result := (ResultCode = 0);
end;

{ Runs right before files are copied, in both wizard and silent mode.
  Cancelling earlier never kills anything: this only executes after the user
  has clicked Install. On failure it reports an error and aborts without
  touching any file. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if FileExists(ExpandConstant('{app}\sodam.exe')) then
  begin
    if (not StopSodamProcess) and SodamExeRunning then
      Result := CustomMessage('SodamStillRunning');
  end;
end;
