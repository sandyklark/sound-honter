; Built by scripts\build.ps1, which supplies AppVersion (from Cargo.toml) and, optionally, Sign.

#ifndef AppVersion
  #error Build with scripts\build.ps1
#endif

#define AppName "Sound Honter"
#define AppExe "soundhonter.exe"
; Shown in Apps > Installed apps.
#define Publisher "Hontworks"

#ifdef Sign
  #define SignFlag "signonce"
#else
  #define SignFlag ""
#endif

[Setup]
; Never change AppId: Windows uses it to recognize upgrades of this app.
AppId={{0D6B3C52-7A41-4E0B-9C1F-5B2E8A96F3D7}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#Publisher}
VersionInfoVersion={#AppVersion}
; Installs for the current user: no admin prompt, and Send To and the right-click menu are per-user anyway.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename=SoundHonter-{#AppVersion}-Setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ChangesAssociations=yes
#ifdef Sign
SignTool=soundhonter
SignedUninstaller=yes
#endif

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion {#SignFlag}
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\third_party\ffmpeg\ffmpeg.exe"; DestDir: "{app}\ffmpeg"; Flags: ignoreversion
Source: "..\third_party\ffmpeg\LICENSE.txt"; DestDir: "{app}\ffmpeg"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autoprograms}\{#AppName} Settings"; Filename: "{app}\{#AppExe}"; Parameters: "settings"
Name: "{usersendto}\Sound Honter Mix"; Filename: "{app}\{#AppExe}"; Parameters: "mix --ask"; WorkingDir: "{app}"
Name: "{usersendto}\Sound Honter Convert to MP3"; Filename: "{app}\{#AppExe}"; Parameters: "convert mp3"; WorkingDir: "{app}"

[Registry]
#define MenuKey "Software\Classes\SystemFileAssociations\audio\shell\SoundHonter"
Root: HKA; Subkey: "{#MenuKey}"; Flags: uninsdeletekey
; Upgrades start from a clean submenu, so entries that were renamed don't linger as duplicates.
Root: HKA; Subkey: "{#MenuKey}\shell"; ValueType: none; Flags: deletekey
Root: HKA; Subkey: "{#MenuKey}"; ValueType: string; ValueName: "MUIVerb"; ValueData: "{#AppName}"
Root: HKA; Subkey: "{#MenuKey}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExe},0"
Root: HKA; Subkey: "{#MenuKey}"; ValueType: string; ValueName: "SubCommands"; ValueData: ""

; Explorer sorts the submenu by key name, so the number prefixes set the order.
#define ConvertVerb(Key, Label, Format) \
  'Root: HKA; Subkey: "' + MenuKey + '\shell\' + Key + '"; ValueType: string; ValueName: "MUIVerb"; ValueData: "' + Label + '"' + NewLine + \
  'Root: HKA; Subkey: "' + MenuKey + '\shell\' + Key + '\command"; ValueType: string; ValueData: """{app}\' + AppExe + '"" convert ' + Format + ' --queue ""%1"""' + NewLine
#emit ConvertVerb("10mp3", "Convert to MP3", "mp3")
#emit ConvertVerb("20wav", "Convert to WAV", "wav")
#emit ConvertVerb("30flac", "Convert to FLAC", "flac")
#emit ConvertVerb("40ogg", "Convert to OGG", "ogg")
#emit ConvertVerb("50m4a", "Convert to M4A", "m4a")

; CommandFlags $20 is ECF_SEPARATORBEFORE.
Root: HKA; Subkey: "{#MenuKey}\shell\60normalize"; ValueType: string; ValueName: "MUIVerb"; ValueData: "Normalize loudness"
Root: HKA; Subkey: "{#MenuKey}\shell\60normalize"; ValueType: dword; ValueName: "CommandFlags"; ValueData: "$20"
Root: HKA; Subkey: "{#MenuKey}\shell\60normalize\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" normalize --queue ""%1"""

Root: HKA; Subkey: "{#MenuKey}\shell\70trim"; ValueType: string; ValueName: "MUIVerb"; ValueData: "Trim silence"
Root: HKA; Subkey: "{#MenuKey}\shell\70trim\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" trim --queue ""%1"""

Root: HKA; Subkey: "{#MenuKey}\shell\80mix"; ValueType: string; ValueName: "MUIVerb"; ValueData: "Mix..."
Root: HKA; Subkey: "{#MenuKey}\shell\80mix"; ValueType: dword; ValueName: "CommandFlags"; ValueData: "$20"
Root: HKA; Subkey: "{#MenuKey}\shell\80mix\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" mix --ask --queue ""%1"""

Root: HKA; Subkey: "{#MenuKey}\shell\90settings"; ValueType: string; ValueName: "MUIVerb"; ValueData: "Settings..."
Root: HKA; Subkey: "{#MenuKey}\shell\90settings"; ValueType: dword; ValueName: "CommandFlags"; ValueData: "$20"
Root: HKA; Subkey: "{#MenuKey}\shell\90settings\command"; ValueType: string; ValueData: """{app}\{#AppExe}"" settings"

[UninstallDelete]
Type: filesandordirs; Name: "{localappdata}\{#AppName}"

[Code]
// Version 0.1.0 installed for all users, with its own right-click entry and uninstaller.
function InitializeSetup(): Boolean;
begin
  Result := True;
  if RegKeyExists(HKLM32, 'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Sound Honter_is1') then
    SuppressibleMsgBox(
      'An older version of Sound Honter is installed.'#13#10#13#10 +
      'To avoid duplicate menu entries, uninstall it from Settings > Apps > Installed apps, ' +
      'then run this setup again if the Send To entries are missing.',
      mbInformation, MB_OK, IDOK);
end;
