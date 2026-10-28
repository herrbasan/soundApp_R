#define MyAppName "SoundApp R"
#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#define MyAppPublisher "David Renelt"
#define MyAppExeName "soundapp_r.exe"
#define MyAppId "{{FF8BFF2E-7D33-44E5-BA21-1E2C06AE8C63}"

[Setup]
AppId={#MyAppId}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={autopf}\SoundApp R
DefaultGroupName=SoundApp R
DisableProgramGroupPage=yes
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=SoundAppR-{#MyAppVersion}-setup
SetupIconFile=..\crates\soundapp\assets\icons\installer.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesAssociations=yes
CloseApplications=yes
RestartApplications=no
VersionInfoVersion={#MyAppVersion}.0
VersionInfoProductName={#MyAppName}
VersionInfoCompany={#MyAppPublisher}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "german"; MessagesFile: "compiler:Languages\German.isl"

[Tasks]
Name: "associate"; Description: "Register SoundApp for supported audio files"; GroupDescription: "Integration:"; Flags: checkedonce
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional icons:"; Flags: unchecked

[Files]
Source: "..\dist\soundapp_r\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent

#include "associations.iss"

; Default Programs registration. The generated OpenWithProgids entries make
; SoundApp available in Explorer's "Open with" list; this capability record also
; exposes it in Windows Settings > Default apps.
Root: HKA; Subkey: "Software\SoundAppR"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\SoundAppR\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "SoundApp R"; Tasks: associate; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\SoundAppR\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Native audio player for FFmpeg formats, tracker modules and MIDI"; Tasks: associate
Root: HKA; Subkey: "Software\SoundAppR\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\{#MyAppExeName},0"; Tasks: associate
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "SoundAppR"; ValueData: "Software\SoundAppR\Capabilities"; Tasks: associate; Flags: uninsdeletevalue

