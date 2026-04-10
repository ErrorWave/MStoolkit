; GPS Log Viewer — Inno Setup installer script
; Created by Owen Hammond & Baker's Communications LLC
;
; Prerequisites:
;   1. Install Inno Setup 6: https://jrsoftware.org/isdl.php
;   2. Build the release binary:  cargo build --release
;   3. Open this file in the Inno Setup IDE and click Compile, or run:
;      "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\setup.iss
;
; Output: installer\dist\GPS_Log_Viewer_Setup_0.1.0.exe

#define MyAppName      "GPS Log Viewer"
#define MyAppVersion   "0.1.0"
#define MyAppPublisher "Baker's Communications LLC"
#define MyAppExeName   "ms_toolkit.exe"
#define MyBinaryDir    "..\target\release"

[Setup]
; Unique application ID – do not reuse across different apps
AppId={{7F3A1B9E-2C4D-4E6F-8A0B-9D1E2F3A4B5C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
; Place the installer .exe in installer\dist\
OutputDir=dist
OutputBaseFilename=GPS_Log_Viewer_Setup_{#MyAppVersion}
; Strong LZMA2 compression — installer will be noticeably smaller
Compression=lzma2/ultra64
SolidCompression=yes
; 64-bit only (Rust target is x86_64-pc-windows-msvc by default)
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
UninstallDisplayIcon={app}\{#MyAppExeName}
; Require elevation so the app installs into Program Files
PrivilegesRequired=admin

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; \
    Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"; \
    Flags: unchecked

[Files]
; Main executable (built with `cargo build --release`)
Source: "{#MyBinaryDir}\{#MyAppExeName}"; \
    DestDir: "{app}"; \
    Flags: ignoreversion

; If your build links the MSVC CRT dynamically you may also need to bundle the
; Visual C++ Redistributable.  The easiest approach is to add a merged-module:
;
;   Source: "C:\Program Files\Microsoft Visual Studio\...\VC_redist.x64.exe"; \
;       DestDir: "{tmp}"; Flags: deleteafterinstall
;
; and a [Run] entry to launch it silently.  If you compiled with
;   [target.x86_64-pc-windows-msvc]
;   rustflags = ["-C", "target-feature=+crt-static"]
; in .cargo\config.toml the CRT is statically linked and no extra step is needed.

[Icons]
Name: "{group}\{#MyAppName}";                          Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}";    Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}";                    Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; \
    Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; \
    Flags: nowait postinstall skipifsilent
