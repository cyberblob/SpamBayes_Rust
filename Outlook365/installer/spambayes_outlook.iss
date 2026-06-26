; SpamBayes Outlook Add-in Installer
; InnoSetup 6 script
; Builds a Windows installer that:
;   - Installs the 64-bit DLL
;   - Registers the COM add-in via regsvr32
;   - Provides clean uninstall

#define MyAppName "SpamBayes Outlook Add-in"
#define MyAppVersion "0.3.0a1"
#define MyAppPublisher "SpamBayes Project"
#define MyAppURL "https://github.com/cyberblob/SpamBayes_Rust"

[Setup]
AppId={{E7F3A2B1-9C4D-4E8F-A1B2-567890ABCDEF}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
DefaultDirName={autopf}\SpamBayes
DefaultGroupName={#MyAppName}
OutputDir=..\installer\output
OutputBaseFilename=SpamBayes_Outlook_Setup_{#MyAppVersion}
Compression=lzma2
SolidCompression=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=yes
CloseApplicationsFilter=OUTLOOK.EXE
UninstallDisplayIcon={app}\spambayes.ico
WizardStyle=modern
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
; 64-bit DLL
Source: "..\target\x86_64-pc-windows-msvc\release\spambayes_addin.dll"; \
    DestDir: "{app}"; DestName: "spambayes_addin.dll"; \
    Flags: ignoreversion regserver 64bit
; 64-bit Manager GUI
Source: "..\target\x86_64-pc-windows-msvc\release\spambayes_manager.exe"; \
    DestDir: "{app}"; DestName: "spambayes_manager.exe"; \
    Flags: ignoreversion
; GTK4 bundle (flat — DLLs next to the add-in DLL for load-time linking)
Source: "..\gtk4-bundle\x64\*.dll"; \
    DestDir: "{app}"; \
    Flags: ignoreversion
; GTK4 data files (schemas, pixbuf loaders)
Source: "..\gtk4-bundle\x64\share\*"; \
    DestDir: "{app}\share"; \
    Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
Source: "..\gtk4-bundle\x64\lib\*"; \
    DestDir: "{app}\lib"; \
    Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
; Toolbar button icons
Source: "..\..\Outlook2000\images\delete_as_spam.bmp"; \
    DestDir: "{app}\images"; Flags: ignoreversion
Source: "..\..\Outlook2000\images\recover_ham.bmp"; \
    DestDir: "{app}\images"; Flags: ignoreversion

[Icons]
Name: "{group}\SpamBayes Manager"; Filename: "{app}\spambayes_manager.exe"
Name: "{group}\Uninstall SpamBayes"; Filename: "{uninstallexe}"

[Code]
// Check if Outlook is running before install
function InitializeSetup(): Boolean;
begin
  Result := True;
  if CheckForMutexes('_Outlook_Mutex_') then
  begin
    if MsgBox('Microsoft Outlook appears to be running.' + #13#10 +
      'Please close Outlook before installing SpamBayes.' + #13#10#13#10 +
      'Click OK to continue anyway, or Cancel to abort.',
      mbConfirmation, MB_OKCANCEL) = IDCANCEL then
      Result := False;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  // No PATH changes needed — the DLL has no GTK4 dependencies.
  // The Manager EXE finds GTK4 DLLs in its own directory.
end;

[UninstallRun]
; Unregister 64-bit DLL
Filename: "{sys}\regsvr32.exe"; Parameters: "/s /u ""{app}\spambayes_addin.dll"""; \
    Flags: 64bit; RunOnceId: "unreg64"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
