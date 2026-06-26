# bundle_gtk4.ps1
# Copies GTK4 runtime DLLs from MSYS2 into the bundle directories.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\bundle_gtk4.ps1
#        powershell -ExecutionPolicy Bypass -File scripts\bundle_gtk4.ps1 -Env ucrt64
#
# Prerequisites:
#   - MSYS2 installed at C:\msys64 (or set $Env:MSYS2_ROOT)
#   - For ucrt64 (default): mingw-w64-ucrt-x86_64-gtk4
#   - For mingw64: mingw-w64-x86_64-gtk4
#   - For mingw32 (32-bit): mingw-w64-i686-gtk4

param(
    [string]$Msys2Root = $(if ($Env:MSYS2_ROOT) { $Env:MSYS2_ROOT } else { "C:\msys64" }),
    [ValidateSet("ucrt64", "mingw64", "mingw32", "auto")]
    [string]$Env = "auto",
    [switch]$X64Only,
    [switch]$X86Only
)

$ErrorActionPreference = "Stop"

# ─── DLL List ─────────────────────────────────────────────────────────────────
# Core GTK4 and all transitive dependencies needed at runtime.
$DllList = @(
    # GTK4 core
    "libgtk-4-1.dll"

    # GLib / GObject / GIO
    "libglib-2.0-0.dll"
    "libgobject-2.0-0.dll"
    "libgio-2.0-0.dll"
    "libgmodule-2.0-0.dll"

    # Graphics / Rendering
    "libcairo-2.dll"
    "libcairo-gobject-2.dll"
    "libcairo-script-interpreter-2.dll"
    "libpango-1.0-0.dll"
    "libpangocairo-1.0-0.dll"
    "libpangoft2-1.0-0.dll"
    "libpangowin32-1.0-0.dll"
    "libgdk_pixbuf-2.0-0.dll"
    "libgraphene-1.0-0.dll"
    "libepoxy-0.dll"
    "libpixman-1-0.dll"

    # Text rendering
    "libharfbuzz-0.dll"
    "libharfbuzz-subset-0.dll"
    "libfribidi-0.dll"
    "libfontconfig-1.dll"
    "libfreetype-6.dll"
    "libgraphite2.dll"
    "libthai-0.dll"
    "libdatrie-1.dll"

    # Image formats
    "libpng16-16.dll"
    "libjpeg-8.dll"
    "libtiff-6.dll"
    "libwebp-7.dll"
    "libsharpyuv-0.dll"
    "libjbig-0.dll"
    "libLerc.dll"
    "libdeflate.dll"

    # GStreamer (media backend for GTK4)
    "libgstreamer-1.0-0.dll"
    "libgstbase-1.0-0.dll"
    "libgstallocators-1.0-0.dll"
    "libgstaudio-1.0-0.dll"
    "libgstvideo-1.0-0.dll"
    "libgstgl-1.0-0.dll"
    "libgstplay-1.0-0.dll"
    "libgstpbutils-1.0-0.dll"
    "libgsttag-1.0-0.dll"
    "libgstd3d12-1.0-0.dll"
    "libgstd3dshader-1.0-0.dll"
    "liborc-0.4-0.dll"

    # Compression / encoding
    "zlib1.dll"
    "libbz2-1.dll"
    "libbrotlidec.dll"
    "libbrotlicommon.dll"
    "liblzma-5.dll"
    "libzstd.dll"
    "liblzo2-2.dll"

    # Internationalization / support
    "libintl-8.dll"
    "libiconv-2.dll"
    "libpcre2-8-0.dll"
    "libffi-8.dll"
    "libexpat-1.dll"

    # Windows C runtime
    "libwinpthread-1.dll"
)

# Architecture-specific DLLs
$X64ExtraDlls = @("libgcc_s_seh-1.dll", "libstdc++-6.dll")
$X86ExtraDlls = @("libgcc_s_dw2-1.dll", "libstdc++-6.dll")

# ─── Helper Functions ─────────────────────────────────────────────────────────

function Copy-GtkBundle {
    param(
        [string]$Arch,       # "x64" or "x86"
        [string]$MingwDir,   # e.g. "mingw64" or "mingw32"
        [string[]]$ExtraDlls
    )

    $SrcBin = Join-Path $Msys2Root "$MingwDir\bin"
    $SrcLib = Join-Path $Msys2Root "$MingwDir\lib"
    $SrcShare = Join-Path $Msys2Root "$MingwDir\share"
    $DestDir = Join-Path $PSScriptRoot "..\gtk4-bundle\$Arch"

    if (-not (Test-Path $SrcBin)) {
        Write-Warning "MSYS2 $MingwDir not found at: $SrcBin"
        Write-Warning "Install the mingw-w64 GTK4 packages first."
        return $false
    }

    Write-Host "=== Bundling GTK4 for $Arch ===" -ForegroundColor Cyan
    Write-Host "    Source: $SrcBin"
    Write-Host "    Dest:   $DestDir"

    # Create destination directory
    if (Test-Path $DestDir) {
        Remove-Item -Recurse -Force $DestDir
    }
    New-Item -ItemType Directory -Path $DestDir -Force | Out-Null

    # Copy DLLs
    $AllDlls = $DllList + $ExtraDlls
    $Copied = 0
    $Missing = @()

    foreach ($dll in $AllDlls) {
        $SrcPath = Join-Path $SrcBin $dll
        if (Test-Path $SrcPath) {
            Copy-Item $SrcPath -Destination $DestDir
            $Copied++
        } else {
            $Missing += $dll
        }
    }

    Write-Host "    Copied $Copied DLLs" -ForegroundColor Green
    if ($Missing.Count -gt 0) {
        Write-Warning "    Missing DLLs (may be optional): $($Missing -join ', ')"
    }

    # Copy GLib schemas
    $SchemaDir = Join-Path $DestDir "share\glib-2.0\schemas"
    $SrcSchemaDir = Join-Path $SrcShare "glib-2.0\schemas"
    if (Test-Path (Join-Path $SrcSchemaDir "gschemas.compiled")) {
        New-Item -ItemType Directory -Path $SchemaDir -Force | Out-Null
        Copy-Item (Join-Path $SrcSchemaDir "gschemas.compiled") -Destination $SchemaDir
        Write-Host "    Copied GLib schemas" -ForegroundColor Green
    }

    # Copy pixbuf loaders
    $LoaderSrcDir = Join-Path $SrcLib "gdk-pixbuf-2.0\2.10.0\loaders"
    $LoaderDestDir = Join-Path $DestDir "lib\gdk-pixbuf-2.0\2.10.0\loaders"
    if (Test-Path $LoaderSrcDir) {
        New-Item -ItemType Directory -Path $LoaderDestDir -Force | Out-Null
        Copy-Item "$LoaderSrcDir\*.dll" -Destination $LoaderDestDir -ErrorAction SilentlyContinue
        # Copy and patch the loaders.cache
        $CacheSrc = Join-Path $SrcLib "gdk-pixbuf-2.0\2.10.0\loaders.cache"
        if (Test-Path $CacheSrc) {
            $CacheDest = Join-Path $DestDir "lib\gdk-pixbuf-2.0\2.10.0\loaders.cache"
            # Read and rewrite paths to be relative
            $content = Get-Content $CacheSrc -Raw
            $content = $content -replace [regex]::Escape("$SrcLib/"), "lib/"
            $content = $content -replace [regex]::Escape("$SrcLib\"), "lib\"
            Set-Content -Path $CacheDest -Value $content
        }
        Write-Host "    Copied pixbuf loaders" -ForegroundColor Green
    }

    # Report bundle size
    $Size = (Get-ChildItem -Recurse $DestDir | Measure-Object -Property Length -Sum).Sum
    $SizeMB = [math]::Round($Size / 1MB, 1)
    Write-Host "    Bundle size: ${SizeMB} MB" -ForegroundColor Yellow
    Write-Host ""

    return $true
}

# ─── Main ─────────────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "GTK4 DLL Bundler for SpamBayes Outlook Add-in" -ForegroundColor White
Write-Host "MSYS2 root: $Msys2Root"

# Auto-detect the 64-bit environment
if ($Env -eq "auto") {
    if (Test-Path (Join-Path $Msys2Root "ucrt64\bin\libgtk-4-1.dll")) {
        $Env = "ucrt64"
    } elseif (Test-Path (Join-Path $Msys2Root "mingw64\bin\libgtk-4-1.dll")) {
        $Env = "mingw64"
    } else {
        Write-Error "Cannot find GTK4 in ucrt64 or mingw64. Install with: pacman -S mingw-w64-ucrt-x86_64-gtk4"
        exit 1
    }
}

Write-Host "Using environment: $Env"
Write-Host ""

$Success = $true

if (-not $X86Only) {
    $MingwDir = if ($Env -eq "ucrt64") { "ucrt64" } else { "mingw64" }
    $ExtraDlls = if ($Env -eq "ucrt64") { $X64ExtraDlls } else { $X64ExtraDlls }
    $result = Copy-GtkBundle -Arch "x64" -MingwDir $MingwDir -ExtraDlls $ExtraDlls
    if (-not $result) { $Success = $false }
}

if (-not $X64Only) {
    $result = Copy-GtkBundle -Arch "x86" -MingwDir "mingw32" -ExtraDlls $X86ExtraDlls
    if (-not $result) { $Success = $false }
}

if ($Success) {
    Write-Host "Done! Bundle directories ready at:" -ForegroundColor Green
    Write-Host "  gtk4-bundle\x64\"
    Write-Host "  gtk4-bundle\x86\"
    Write-Host ""
    Write-Host "These will be included by the InnoSetup installer automatically."
} else {
    Write-Host "Some architectures could not be bundled. See warnings above." -ForegroundColor Red
    exit 1
}
