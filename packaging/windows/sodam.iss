; SodaM Windows installer script (Inno Setup 6).
; Build (from repo root):
;   ISCC.exe /DMyAppVersion=<version> packaging/windows/sodam.iss
; Output: release/sodam-<version>-windows-x64-setup.exe
; Per-user install (no admin required), default dir {localappdata}\Programs\SodaM.
; NOTE: keep this file ASCII-only; Inno reads non-BOM files in the ANSI codepage.

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
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\sodam.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\SodaM"; Filename: "{app}\sodam.exe"
Name: "{autodesktop}\SodaM"; Filename: "{app}\sodam.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\sodam.exe"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
