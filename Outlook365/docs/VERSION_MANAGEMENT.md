# Version Management

All version bumps are handled by a single script: `scripts/bump_version.ps1`.

## Quick Usage

```powershell
cd Outlook365
.\scripts\bump_version.ps1 -Version "0.3.0-alpha.3"
```

With a custom release date:

```powershell
.\scripts\bump_version.ps1 -Version "1.0.0" -ReleaseDate "2026-12-25"
```

If `-ReleaseDate` is omitted, it defaults to today.

## What the Script Updates

| # | File | Field / Pattern | Format |
|---|------|-----------------|--------|
| 1 | `Cargo.toml` | `[workspace.package] version` | Semver (`0.3.0-alpha.3`) |
| 2 | `installer/spambayes_outlook.iss` | `#define MyAppVersion` | PEP 440 (`0.3.0a3`) |
| 3 | `installer/version_manifest.json` | `version` + `release_date` | Semver + ISO date |
| 4 | Root `README.md` | `**Version:**` line + installer filename | Both formats |
| 5 | `installer/README.md` | Installer output filename | PEP 440 |
| 6 | `crates/spambayes-addin/src/version_manifest.rs` | Doc comment examples | Both formats |

## Version Format Conventions

The project uses two version formats depending on context:

| Context | Format | Example |
|---------|--------|---------|
| Cargo/Rust (semver) | `MAJOR.MINOR.PATCH-prerelease.N` | `0.3.0-alpha.3` |
| Inno Setup / filenames (PEP 440) | `MAJOR.MINOR.PATCHpreN` | `0.3.0a3` |

The script automatically converts between them:

| Semver Input | PEP 440 Output |
|--------------|----------------|
| `0.3.0-alpha.3` | `0.3.0a3` |
| `0.3.0-beta.1` | `0.3.0b1` |
| `0.3.0-rc.2` | `0.3.0rc2` |
| `0.3.0` | `0.3.0` |

## How Runtime Code Gets the Version

The version flows from `Cargo.toml` to runtime code at compile time:

```
Cargo.toml (workspace.package.version)
    │
    ▼  inherited via version.workspace = true
All 5 crate Cargo.toml files
    │
    ▼  build.rs reads CARGO_PKG_VERSION
crates/spambayes-addin/build.rs
    │
    ▼  emits cargo:rustc-env=SPAMBAYES_VERSION=...
env!("SPAMBAYES_VERSION") in version_manifest.rs
env!("CARGO_PKG_VERSION") in GUI code (About dialog, General tab)
```

This means the Rust source code never contains a hard-coded version string. Changing `Cargo.toml` (via the bump script) propagates everywhere automatically on the next build.

## Typical Release Workflow

```powershell
# 1. Bump version
.\scripts\bump_version.ps1 -Version "0.3.0-beta.1"

# 2. Review what changed
git diff

# 3. Build and test
build_all.bat
cargo test --target x86_64-pc-windows-msvc

# 4. Commit
git add Cargo.toml installer/ README.md crates/spambayes-addin/src/version_manifest.rs
git commit -m "Bump version to 0.3.0-beta.1"

# 5. Tag
git tag v0.3.0-beta.1
```

## Adding New Version Locations

If you add a new file that contains a version string (e.g., a new manifest, a changelog, or a config file), add a corresponding section to `scripts/bump_version.ps1` following the existing pattern:

```powershell
# ─── N. path/to/new_file ──────────────────────────────────────────────────
$newFile = "path\to\new_file"
if (Test-Path $newFile) {
    $content = Get-Content $newFile -Raw
    $content = $content -replace '<regex matching old version>', "<replacement with $Version>"
    Set-Content $newFile $content -NoNewline
    Write-Host "  [OK] $newFile → $Version" -ForegroundColor Green
}
```

Then update the table in this document.
