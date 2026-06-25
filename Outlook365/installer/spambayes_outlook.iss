; SpamBayes Outlook Add-in Installer
; InnoSetup 6 script
; Builds a Windows installer that:
;   - Detects 32-bit vs 64-bit Outlook
;   - Installs the correct DLL
;   - Registers the COM add-in via regsvr32
;   - Provides clean uninstall

#define MyAppName "SpamBayes Outlook Add-in"
#define MyAppVersion "0.1.0"
#define MyAppPublisher "SpamBayes Project"
#define MyAppURL "https://github.com/spambayes"

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
ArchitecturesAllowed=x86compatible x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
CloseApplications=yes
CloseApplicationsFilter=OUTLOOK.EXE
UninstallDisplayIcon={app}\spambayes.ico
WizardStyle=modern
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Types]
Name: "auto"; Description: "Automatic (detect Outlook bitness)"
Name: "x86"; Description: "32-bit only"
Name: "x64"; Description: "64-bit only"

[Components]
Name: "addin32"; Description: "32-bit Outlook Add-in"; Types: auto x86; Check: IsOutlook32BitOrAuto
Name: "addin64"; Description: "64-bit Outlook Add-in"; Types: auto x64; Check: IsOutlook64BitOrAuto

[Files]
; 32-bit DLL
Source: "..\target\i686-pc-windows-msvc\release\spambayes_addin.dll"; \
    DestDir: "{app}\x86"; DestName: "spambayes_addin.dll"; \
    Components: addin32; Flags: ignoreversion regserver 32bit
; 64-bit DLL
Source: "..\target\x86_64-pc-windows-msvc\release\spambayes_addin.dll"; \
    DestDir: "{app}\x64"; DestName: "spambayes_addin.dll"; \
    Components: addin64; Flags: ignoreversion regserver 64bit

[Icons]
Name: "{group}\Uninstall SpamBayes"; Filename: "{uninstallexe}"

[Code]
// Detect whether Outlook is 32-bit or 64-bit by reading registry
function GetOutlookBitness(): Integer;
var
  Bitness: String;
begin
  Result := 0; // Unknown
  // Office 2016+ / Microsoft 365
  if RegQueryStringValue(HKEY_LOCAL_MACHINE,
    'SOFTWARE\Microsoft\Office\ClickToRun\Configuration',
    'Platform', Bitness) then
  begin
    if Bitness = 'x86' then Result := 32
    else if Bitness = 'x64' then Result := 64;
    Exit;
  end;
  // Office MSI installation - check for 64-bit Outlook
  if RegKeyExists(HKEY_LOCAL_MACHINE,
    'SOFTWARE\Microsoft\Office\16.0\Outlook') then
  begin
    if IsWin64 then
    begin
      if RegKeyExists(HKEY_LOCAL_MACHINE,
        'SOFTWARE\WOW6432Node\Microsoft\Office\16.0\Outlook') then
        Result := 32
      else
        Result := 64;
    end else
      Result := 32;
    Exit;
  end;
  // Fallback: check for older Office versions
  if RegKeyExists(HKEY_LOCAL_MACHINE,
    'SOFTWARE\Microsoft\Office\15.0\Outlook') then
  begin
    Result := 32; // Office 2013 was predominantly 32-bit
    Exit;
  end;
end;

function IsOutlook32BitOrAuto(): Boolean;
var
  Bits: Integer;
begin
  Bits := GetOutlookBitness();
  Result := (Bits = 32) or (Bits = 0); // Default to 32-bit if unknown
end;

function IsOutlook64BitOrAuto(): Boolean;
var
  Bits: Integer;
begin
  Bits := GetOutlookBitness();
  Result := (Bits = 64);
end;

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

// Show detected Outlook bitness on the ready page
function UpdateReadyMemo(Space, NewLine, MemoUserInfoInfo, MemoDirInfo,
  MemoTypeInfo, MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: String): String;
var
  Bits: Integer;
begin
  Bits := GetOutlookBitness();
  Result := '';
  if Bits = 32 then
    Result := Result + 'Detected Outlook: 32-bit' + NewLine
  else if Bits = 64 then
    Result := Result + 'Detected Outlook: 64-bit' + NewLine
  else
    Result := Result + 'Detected Outlook: Unknown (will install 32-bit)' + NewLine;
  Result := Result + NewLine;
  if MemoDirInfo <> '' then
    Result := Result + MemoDirInfo + NewLine + NewLine;
  if MemoComponentsInfo <> '' then
    Result := Result + MemoComponentsInfo + NewLine + NewLine;
end;

[UninstallRun]
; Unregister 32-bit DLL
Filename: "{sys}\regsvr32.exe"; Parameters: "/s /u ""{app}\x86\spambayes_addin.dll"""; \
    Flags: 32bit; Components: addin32; RunOnceId: "unreg32"
; Unregister 64-bit DLL
Filename: "{sys}\regsvr32.exe"; Parameters: "/s /u ""{app}\x64\spambayes_addin.dll"""; \
    Flags: 64bit; Components: addin64; RunOnceId: "unreg64"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
