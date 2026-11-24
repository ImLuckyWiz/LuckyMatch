#ifndef AppVersion
  #define AppVersion "0.1.1"
#endif

[Setup]
AppId={{7D5D9B84-2E9E-4D1F-9B61-2C6C69D0B4A1}
AppName=LuckyMatch
AppVersion={#AppVersion}
AppPublisher=ImLuckyWiz
AppPublisherURL=https://github.com/ImLuckyWiz/LuckyMatch
AppSupportURL=https://github.com/ImLuckyWiz/LuckyMatch/issues
DefaultDirName={autopf}\LuckyMatch
DefaultGroupName=LuckyMatch
AllowNoIcons=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\..\assets\branding\luckymatch.ico
UninstallDisplayIcon={app}\luckymatch-desktop.exe
OutputDir=..\..\dist
OutputBaseFilename=luckymatch-setup-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\..\dist\luckymatch-windows-x64\luckymatch-desktop.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\luckymatch-windows-x64\luckymatch-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\luckymatch-windows-x64\config.example.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\luckymatch-windows-x64\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\LuckyMatch"; Filename: "{app}\luckymatch-desktop.exe"
Name: "{group}\Uninstall LuckyMatch"; Filename: "{uninstallexe}"
Name: "{autodesktop}\LuckyMatch"; Filename: "{app}\luckymatch-desktop.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\luckymatch-desktop.exe"; Description: "{cm:LaunchProgram,LuckyMatch}"; Flags: nowait postinstall skipifsilent
