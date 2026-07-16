<#
.SYNOPSIS
    Bumps the SpamBayes version across all locations in the workspace.

.DESCRIPTION
    Updates the version string in:
      - Cargo.toml (workspace.package.version) — the single source of truth
      - installer/spambayes_outlook.iss (#define MyAppVersion, PEP440 format)
      - installer/version_manifest.json (version field + release_date)
      - Root README.md (Version line + installer output filename)
      - installer/README.md (installer output filename)
      - crates/spambayes-addin/src/version_manifest.rs (doc comment examples)

.PARAMETER Version
    Semantic version string, e.g. "0.3.0-alpha.3", "0.3.0-rc.1", "0.3.0"

.PARAMETER ReleaseDate
    Optional ISO 8601 date for the version manifest. Defaults to today.

.EXAMPLE
    .\scripts\bump_version.ps1 -Version "0.3.0-alpha.3"
    .\scripts\bump_version.ps1 -Version "1.0.0" -ReleaseDate "2026-12-25"
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$Version,

    [string]$ReleaseDate = (Get-Date -Format "yyyy-MM-dd")
)

$ErrorActionPreference = "Stop"

# Resolve workspace root (one level up from scripts/)
$WorkspaceRoot = Split-Path -Parent $PSScriptRoot
Push-Location $WorkspaceRoot

try {
    # ─── Helper: Convert semver pre-release to PEP 440 format ─────────────────
    # "0.3.0-alpha.3" -> "0.3.0a3"
    # "0.3.0-beta.1"  -> "0.3.0b1"
    # "0.3.0-rc.2"    -> "0.3.0rc2"
    # "0.3.0"         -> "0.3.0"
    function ConvertTo-Pep440 {
        param([string]$SemVer)

        if ($SemVer -match '^(\d+\.\d+\.\d+)-alpha\.(\d+)$') {
            return "$($Matches[1])a$($Matches[2])"
        }
        elseif ($SemVer -match '^(\d+\.\d+\.\d+)-beta\.(\d+)$') {
            return "$($Matches[1])b$($Matches[2])"
        }
        elseif ($SemVer -match '^(\d+\.\d+\.\d+)-rc\.(\d+)$') {
            return "$($Matches[1])rc$($Matches[2])"
        }
        else {
            # Stable release — no suffix
            return $SemVer
        }
    }

    $Pep440Version = ConvertTo-Pep440 -SemVer $Version

    Write-Host "Bumping version to: $Version (PEP 440: $Pep440Version)" -ForegroundColor Cyan
    Write-Host ""

    # ─── 1. Cargo.toml ────────────────────────────────────────────────────────
    $cargoToml = "Cargo.toml"
    $content = Get-Content $cargoToml -Raw
    $content = $content -replace '(?m)^(version\s*=\s*")[^"]+(")', "`${1}$Version`${2}"
    Set-Content $cargoToml $content -NoNewline
    Write-Host "  [OK] $cargoToml -> $Version" -ForegroundColor Green

    # ─── 2. installer/spambayes_outlook.iss ───────────────────────────────────
    $issFile = "installer\spambayes_outlook.iss"
    $content = Get-Content $issFile -Raw
    $content = $content -replace '(#define MyAppVersion\s+")[^"]+(")', "`${1}$Pep440Version`${2}"
    Set-Content $issFile $content -NoNewline
    Write-Host "  [OK] $issFile -> $Pep440Version" -ForegroundColor Green

    # ─── 3. installer/version_manifest.json ───────────────────────────────────
    $manifestFile = "installer\version_manifest.json"
    $manifest = Get-Content $manifestFile -Raw | ConvertFrom-Json
    $manifest.version = $Version
    $manifest.release_date = $ReleaseDate
    $manifest | ConvertTo-Json -Depth 10 | Set-Content $manifestFile
    Write-Host "  [OK] $manifestFile -> $Version (date: $ReleaseDate)" -ForegroundColor Green

    # ─── 4. Root README.md ────────────────────────────────────────────────────
    $readmeRoot = Join-Path $WorkspaceRoot "..\README.md"
    if (Test-Path $readmeRoot) {
        $content = Get-Content $readmeRoot -Raw
        # Update "**Version:** X.Y.Z-pre.N"
        $content = $content -replace '(\*\*Version:\*\*\s+)\S+', "`${1}$Version"
        # Update installer output filename references
        $content = $content -replace 'SpamBayes_Outlook_Setup_[^\s"''`]+\.exe', "SpamBayes_Outlook_Setup_$Pep440Version.exe"
        Set-Content $readmeRoot $content -NoNewline
        Write-Host "  [OK] README.md (root) -> $Version" -ForegroundColor Green
    }

    # ─── 5. installer/README.md ───────────────────────────────────────────────
    $readmeInstaller = "installer\README.md"
    if (Test-Path $readmeInstaller) {
        $content = Get-Content $readmeInstaller -Raw
        $content = $content -replace 'SpamBayes_Outlook_Setup_[^\s"''`]+\.exe', "SpamBayes_Outlook_Setup_$Pep440Version.exe"
        Set-Content $readmeInstaller $content -NoNewline
        Write-Host "  [OK] $readmeInstaller -> $Pep440Version" -ForegroundColor Green
    }

    # ─── 6. version_manifest.rs doc comment examples ──────────────────────────
    $manifestRs = "crates\spambayes-addin\src\version_manifest.rs"
    if (Test-Path $manifestRs) {
        $content = Get-Content $manifestRs -Raw
        # Update example JSON version field: "version": "X.Y.Z-pre.N"
        $content = $content -replace '("version":\s*")\d+\.\d+\.\d+[^"]*(")', "`${1}$Version`${2}"
        # Update installer_url version in the example (download/vX.Y.Z-pre.N/)
        $replacement = "`${1}$Version`${2}"
        $content = $content -replace '(download/v)\d+\.\d+\.\d+[^/]*(/)', $replacement
        # Update installer exe filename in the example
        $content = $content -replace 'SpamBayes_Outlook_Setup_[^"]+\.exe', "SpamBayes_Outlook_Setup_$Pep440Version.exe"
        # Update doc comment "e.g." references
        $content = $content -replace '(e\.g\.,\s*")\d+\.\d+\.\d+-alpha\.\d+(")', "`${1}$Version`${2}"
        Set-Content $manifestRs $content -NoNewline
        Write-Host "  [OK] $manifestRs -> $Version" -ForegroundColor Green
    }

    Write-Host ""
    Write-Host "Version bump complete! All files updated to $Version." -ForegroundColor Cyan
    Write-Host ""
    Write-Host "Next steps:" -ForegroundColor Yellow
    Write-Host "  1. Review changes: git diff"
    Write-Host "  2. Build: build_all.bat"
    Write-Host "  3. Test: cargo test --target x86_64-pc-windows-msvc"
    Write-Host "  4. Commit: git add -A && git commit -m 'Bump version to $Version'"
}
finally {
    Pop-Location
}
