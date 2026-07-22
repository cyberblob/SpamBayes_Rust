# SpamBayes Outlook Add-in Installer

## Prerequisites

- MSYS2 with GTK4 installed: `pacman -S mingw-w64-ucrt-x86_64-gtk4`
- Rust toolchain with `x86_64-pc-windows-msvc` target
- 64-bit Outlook (Microsoft 365 / Office 2019+)

## Quick Build + Install

```cmd
cd Outlook365

REM 1. Build the DLL (64-bit by default)
build_all.bat

REM 2. Bundle GTK4 runtime DLLs
powershell -ExecutionPolicy Bypass -File scripts\bundle_gtk4.ps1

REM 3. Install (right-click → Run as administrator)
installer\install.bat
```

## Build Options

```cmd
build_all.bat            # 64-bit only (default, recommended)
build_all.bat --both     # Both 32-bit and 64-bit (requires mingw32 GTK4)
build_all.bat --x86      # 32-bit only (requires mingw32 GTK4)
```

The 32-bit build requires `mingw-w64-i686-gtk4` installed in MSYS2. Since Microsoft 365 defaults to 64-bit Outlook, the 32-bit target is only needed for legacy installations.

## Quick Uninstall

Right-click `uninstall.bat` → **Run as administrator** (also available at `C:\Program Files\SpamBayes\uninstall.bat`)

## InnoSetup Installer (for distribution)

For a proper Windows Setup wizard with Add/Remove Programs integration:

1. Install [Inno Setup 6](https://jrsoftware.org/isinfo.php)
2. Build the DLL: `build_all.bat`
3. Bundle GTK4: `powershell -ExecutionPolicy Bypass -File scripts\bundle_gtk4.ps1`
4. Compile the installer:
   ```
   "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\spambayes_outlook.iss
   ```
5. Output: `installer\output\SpamBayes_Outlook_Setup_0.3.0a4.exe`

### What the InnoSetup installer does

- Detects Outlook bitness (32-bit vs 64-bit) automatically
- Warns if Outlook is running
- Copies the correct DLL to `C:\Program Files\SpamBayes\`
- Bundles GTK4 runtime DLLs alongside the add-in
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

## Troubleshooting

**Build fails with "pkg-config has not been configured to support cross-compilation"**
- This means you're trying to build 32-bit without 32-bit GTK4 libraries
- Fix: use `build_all.bat` (64-bit only) or install `mingw-w64-i686-gtk4`

**GTK4 DLLs not found at runtime**
- Run `scripts\bundle_gtk4.ps1` to copy GTK4 DLLs into the bundle directory
- The installer copies these alongside the add-in DLL

**Outlook doesn't show the add-in after install**
- Restart Outlook completely (check Task Manager for lingering processes)
- Verify COM registration: check for `SpamBayes.OutlookAddin` in registry
- Check Outlook's disabled add-ins list: File → Options → Add-ins → Manage COM Add-ins
