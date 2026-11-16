; Inno Setup script for the SimQ installer on Windows.
;
; Built by the release workflow:
;   iscc /DAppVersion=1.2.3 installer\simq.iss
;
; The app is installed for the current user only, without administrator
; rights, so that the in-app update can replace the program later.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{8E7A3B52-5D0C-4E0B-9C1E-51D3A9F0C2B7}
AppName=SimQ
AppVersion={#AppVersion}
AppPublisher=SimQ
AppPublisherURL=https://github.com/Olsn-Dev/simq
DefaultDirName={localappdata}\Programs\SimQ
DefaultGroupName=SimQ
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
OutputDir=..
OutputBaseFilename=simq-v{#AppVersion}-windows-x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\SimQ.exe
UninstallDisplayName=SimQ
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\simq.exe"; DestDir: "{app}"; DestName: "SimQ.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\SimQ"; Filename: "{app}\SimQ.exe"
Name: "{autodesktop}\SimQ"; Filename: "{app}\SimQ.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\SimQ.exe"; Description: "{cm:LaunchProgram,SimQ}"; Flags: nowait postinstall skipifsilent
