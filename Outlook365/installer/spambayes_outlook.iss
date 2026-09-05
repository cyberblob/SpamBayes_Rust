; SpamBayes Outlook Add-in Installer
; InnoSetup 6 script
; Builds a Windows installer that:
;   - Installs the 64-bit DLL
;   - Registers the COM add-in via regsvr32
;   - Provides clean uninstall

#define MyAppName "SpamBayes Outlook Add-in"
#define MyAppVersion "0.3.0a8"
; Canonical SemVer string that matches the DLL's compiled-in CARGO_PKG_VERSION
; (env!("SPAMBAYES_VERSION")). Must be kept in sync with the workspace
; Cargo.toml `version` field. Used to stamp install_target_version into the
; user's INI so the add-in can detect a pending update that did not load.
#define MyAppSemVer "0.3.0-alpha.6"
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
; restartreplace + uninsrestartdelete: if Outlook still has the DLL loaded
; (file locked), schedule the replacement on next reboot instead of silently
; leaving the old DLL in place — otherwise the add-in keeps reporting the old
; version after a manual install.
Source: "..\target\x86_64-pc-windows-msvc\release\spambayes_addin.dll"; \
    DestDir: "{app}"; DestName: "spambayes_addin.dll"; \
    Flags: ignoreversion regserver 64bit restartreplace uninsrestartdelete
; 64-bit Manager GUI
Source: "..\target\x86_64-pc-windows-msvc\release\spambayes_manager.exe"; \
    DestDir: "{app}"; DestName: "spambayes_manager.exe"; \
    Flags: ignoreversion
; 64-bit Clues Viewer
Source: "..\target\x86_64-pc-windows-msvc\release\spambayes_clues.exe"; \
    DestDir: "{app}"; DestName: "spambayes_clues.exe"; \
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

[INI]
; Stamp the version this installer is placing on disk into the user's profile
; INI. On next Outlook startup the add-in compares this against the version
; actually running (CURRENT_VERSION). If they differ, the new DLL did not load
; (e.g. it was locked by a running Outlook) and the add-in reports that a
; restart/reboot is needed to finish the update.
;
; Note: {localappdata} resolves to the *installing* user's profile. For a
; per-machine (admin) install used by a different user, that user's add-in will
; simply re-stamp installed_version on its own first startup, which still keeps
; the mirror correct — it just won't have the target-version pending check.
Filename: "{localappdata}\SpamBayes\default.ini"; Section: "Update"; \
    Key: "install_target_version"; String: "{#MyAppSemVer}"; \
    Flags: uninsdeleteentry

[Code]
// Check if Outlook is running before install.
//
// The add-in DLL is memory-mapped by OUTLOOK.EXE while it is running, so
// Windows refuses to overwrite it. If we let the install proceed with Outlook
// open, the new DLL is NOT copied and the add-in keeps loading (and reporting)
// the OLD version. We therefore block the install until Outlook is closed
// rather than merely warning. CloseApplications=yes will also attempt to close
// it automatically, and restartreplace handles the edge case where a handle
// lingers.
function InitializeSetup(): Boolean;
begin
  Result := True;
  if CheckForMutexes('_Outlook_Mutex_') then
  begin
    if MsgBox('Microsoft Outlook is currently running.' + #13#10 +
      'SpamBayes cannot update its add-in while Outlook has it open, and ' +
      'the update will not take effect until Outlook is closed.' + #13#10#13#10 +
      'Please close Outlook completely, then click Retry.' + #13#10 +
      'Click Cancel to abort the installation.',
      mbError, MB_RETRYCANCEL) = IDCANCEL then
    begin
      Result := False;
      Exit;
    end;
    // Re-check after the user says they've closed it. If it is still running,
    // abort so we never silently leave the old DLL in place.
    if CheckForMutexes('_Outlook_Mutex_') then
    begin
      MsgBox('Outlook still appears to be running. Installation aborted.' + #13#10 +
        'Close Outlook (check the system tray and Task Manager for ' +
        'OUTLOOK.EXE) and run the installer again.',
        mbError, MB_OK);
      Result := False;
    end;
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
