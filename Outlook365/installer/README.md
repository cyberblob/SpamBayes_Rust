# SpamBayes Outlook Add-in Installer

## Quick Install (batch file)

1. Build both DLLs first: run `build_all.bat` from the `Outlook365/` directory
2. Right-click `install.bat` → **Run as administrator**
3. Restart Outlook

The installer auto-detects whether your Outlook is 32-bit or 64-bit and registers the correct DLL.

## Quick Uninstall

Right-click `uninstall.bat` → **Run as administrator** (also available at `C:\Program Files\SpamBayes\uninstall.bat`)

## InnoSetup Installer (for distribution)

For a proper Windows Setup wizard with Add/Remove Programs integration:

1. Install [Inno Setup 6](https://jrsoftware.org/isinfo.php)
2. Build both DLLs: `build_all.bat`
3. Compile the installer:
   ```
   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\spambayes_outlook.iss
   ```
4. Output: `installer\output\SpamBayes_Outlook_Setup_0.1.0.exe`

### What the InnoSetup installer does

- Detects Outlook bitness (32-bit vs 64-bit) automatically
- Warns if Outlook is running
- Copies the correct DLL to `C:\Program Files\SpamBayes\`
- Registers the COM DLL via `regsvr32`
- Adds an entry in Windows Add/Remove Programs
- Clean uninstall (deregisters COM, removes files, cleans registry)

## Manual Registration

If you prefer to register/unregister the DLL manually:

```cmd
REM Register (as admin)
regsvr32 "path\to\spambayes_addin.dll"

REM Unregister (as admin)
regsvr32 /u "path\to\spambayes_addin.dll"
```

## Registry Entries Created

The add-in creates these registry entries when registered:

| Key | Purpose |
|-----|---------|
| `HKCR\CLSID\{A3B9E8D1-4F2C-4A6E-B8D7-1234567890AB}` | COM class registration |
| `HKCR\CLSID\{...}\InprocServer32` | DLL path + threading model |
| `HKCR\CLSID\{...}\ProgID` | SpamBayes.OutlookAddin |
| `HKCU\Software\Microsoft\Office\Outlook\Addins\SpamBayes.OutlookAddin` | Outlook add-in discovery |
