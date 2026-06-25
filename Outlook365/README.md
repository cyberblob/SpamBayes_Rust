# SpamBayes Outlook Add-in (Rust)

A native Rust implementation of the SpamBayes Outlook COM add-in. This is a ground-up rewrite of the original Python SpamBayes Outlook2000 plugin, compiled to a standalone DLL that loads directly into Microsoft Outlook without requiring a Python runtime.

## Overview

SpamBayes uses a Bayesian statistical classifier to sort email into ham (good), spam, or unsure. This Rust version provides:

- **Native COM add-in** — loads as an in-process DLL, no Python or runtime dependencies
- **Real-time folder monitoring** — watches configured folders (e.g. Junk Email) via Outlook's `Items.ItemAdd` COM events
- **Startup scan** — processes existing unscored messages on launch
- **Automatic scoring and filtering** — classifies messages and moves them to configured spam/unsure folders
- **Ribbon UI integration** — Spam, Not Spam, and Manager buttons in Outlook's toolbar
- **Configuration migration** — reads existing Python SpamBayes INI files and database
- **Dual-architecture** — builds both 32-bit and 64-bit DLLs for any Outlook version
- **Zero-dependency installer** — InnoSetup-based, auto-detects Outlook bitness

## Architecture

The project is organized as a Cargo workspace with five crates:

```
Outlook365/
├── Cargo.toml              # Workspace root
├── build_all.bat           # Dual-arch build script
├── .cargo/config.toml      # Static CRT linking config
│
├── crates/
│   ├── spambayes-core/     # Classifier + tokenizer (pure Rust, no Windows deps)
│   ├── spambayes-storage/  # Database persistence (mmap-backed dbm)
│   ├── spambayes-config/   # INI parsing, typed FolderIds, config structs
│   ├── spambayes-mapi/     # MAPI session, message store, folder access
│   └── spambayes-addin/    # COM DLL: IDTExtensibility2, ribbon, filter, events
│
└── installer/
    ├── spambayes_outlook.iss   # InnoSetup script
    └── output/                 # Built installer EXE
```

### Crate Responsibilities

| Crate | Purpose |
|-------|---------|
| `spambayes-core` | Bayesian classifier, chi-squared combining, email tokenizer. Platform-independent. |
| `spambayes-storage` | Mmap-backed token database, message metadata DB, Python DB migration. |
| `spambayes-config` | INI file parsing, `AppConfig` structs, `FolderId` types, Python config migration. |
| `spambayes-mapi` | Windows MAPI session management, message store operations, folder enumeration. |
| `spambayes-addin` | COM add-in entry point (`DllGetClassObject`, `IDTExtensibility2`), ribbon XML, toolbar, folder event sinks, filter engine orchestration, training engine, notification manager. |

## Building

### Prerequisites

- Rust toolchain (stable) with both targets:
  ```
  rustup target add i686-pc-windows-msvc
  rustup target add x86_64-pc-windows-msvc
  ```
- Visual Studio Build Tools (for MSVC linker)
- InnoSetup 6 (for installer, optional)

### Build Commands

```bash
# From the Outlook365/ directory:

# Build both architectures (recommended)
build_all.bat

# Or build individually:
cargo build --release --target x86_64-pc-windows-msvc   # 64-bit
cargo build --release --target i686-pc-windows-msvc     # 32-bit

# Run tests (214 tests across all crates)
cargo test

# Build installer
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\spambayes_outlook.iss
```

### Output

```
target/x86_64-pc-windows-msvc/release/spambayes_addin.dll   # 64-bit COM DLL
target/i686-pc-windows-msvc/release/spambayes_addin.dll     # 32-bit COM DLL
target/x86_64-pc-windows-msvc/release/spambayes_manager.exe # Manager launcher
installer/output/SpamBayes_Outlook_Setup_*.exe              # Installer
```

## Installation

### Via Installer (Recommended)

Run `SpamBayes_Outlook_Setup_*.exe` as Administrator. It will:
1. Detect Outlook bitness (32 or 64-bit)
2. Install the correct DLL to `C:\Program Files\SpamBayes\`
3. Register the COM add-in via `regsvr32`
4. Outlook will load SpamBayes on next startup

### Manual Registration

```cmd
regsvr32 "C:\Program Files\SpamBayes\x64\spambayes_addin.dll"
```

To unregister:
```cmd
regsvr32 /u "C:\Program Files\SpamBayes\x64\spambayes_addin.dll"
```

## Configuration

Configuration is stored in `%APPDATA%\SpamBayes\Outlook.ini` (Python format) and migrated automatically on first run. Key settings:

| Section | Key | Description |
|---------|-----|-------------|
| `[Filter]` | `enabled` | Enable/disable real-time filtering |
| `[Filter]` | `watch_folder_ids` | Folders to monitor for new messages |
| `[Filter]` | `spam_folder_id` | Destination for classified spam |
| `[Filter]` | `unsure_folder_id` | Destination for unsure messages |
| `[Filter]` | `spam_threshold` | Score % at/above which = spam (default: 90) |
| `[Filter]` | `unsure_threshold` | Score % at/above which = unsure (default: 15) |
| `[Filter]` | `timer_enabled` | Background timer-based filtering |
| `[Filter]` | `timer_start_delay` | Seconds before first timer fires |
| `[Filter]` | `timer_interval` | Seconds between timer ticks |

The Manager GUI (launched from the ribbon) provides a visual interface for all settings.

## How It Works

### Startup Sequence

1. Outlook loads `spambayes_addin.dll` via COM (`DllGetClassObject` → `IClassFactory` → `AddinCore`)
2. `OnConnection` — stores Application pointer, loads config, initializes MAPI, loads classifier database
3. `OnStartupComplete` — defers toolbar and folder hook setup via Windows timers (avoids COM reentrancy)
4. Toolbar timer (1.5s) — creates CommandBar buttons via Outlook Object Model
5. Folder hook timer (2.5s) — resolves watch folders, connects `ItemAdd` event sinks, scans existing items

### Message Processing Flow

```
New message arrives in watched folder
        │
        ▼
ItemAdd event fires on Items collection
        │
        ▼
Extract message content (MIME or Headers+Body via PropertyAccessor)
        │
        ▼
Tokenize → Score with Bayesian classifier (chi-squared combining)
        │
        ▼
Classify: Score ≥ spam_threshold → Spam
          Score ≥ unsure_threshold → Unsure
          Otherwise → Ham
        │
        ▼
Perform action: Move/Copy to configured folder, mark as read, save score
```

### COM Registration

The DLL registers under:
- **CLSID**: `{A3B9E8D1-4F2C-4A6E-B8D7-1234567890AB}`
- **ProgID**: `SpamBayes.OutlookAddin`
- **Outlook Addins**: `HKCU\Software\Microsoft\Office\Outlook\Addins\SpamBayes.OutlookAddin`
- **LoadBehavior**: 3 (load at startup)
- **Threading Model**: Apartment (STA)

## Logging

The add-in writes diagnostic logs to:

| Log File | Location | Content |
|----------|----------|---------|
| `addin_debug.log` | `%LOCALAPPDATA%\SpamBayes\` | COM lifecycle, toolbar, timer events |
| `folder_monitor.log` | `%LOCALAPPDATA%\SpamBayes\` | Folder hooks, message classification, moves |

Example `folder_monitor.log` output:
```
[15:30:45] ========== ItemAdd FIRED in 'Junk Email' ==========
[15:30:45]   Subject: Win a free iPhone!!!
[15:30:45]   Sender: spammer@evil.com
[15:30:45]   MessageClass: IPM.Note
[15:30:45]   Content source: Headers (2048 bytes) + Body (512 bytes)
[15:30:45]   *** CLASSIFICATION: Spam ***
[15:30:45]   Score: 87.34%
[15:30:45]   Spam threshold: 18.90%, Unsure threshold: 13.40%
[15:30:45]   Action: MOVE to Spam folder (entry=0000000097D1...)
[15:30:45]   Processing COMPLETE for 'Win a free iPhone!!!'
```

## Security

- **Snyk SAST**: 0 issues found in Rust code
- **Static CRT linking**: No runtime DLL dependencies
- **Memory safety**: Rust ownership model prevents buffer overflows and use-after-free in classifier/tokenizer code
- **COM safety**: Unsafe blocks are confined to COM vtable implementations with documented invariants

## Migration from Python SpamBayes

The Rust add-in automatically detects and migrates:
- **Configuration** — reads `Outlook.ini` from `%APPDATA%\SpamBayes\`, preserves all folder IDs and thresholds
- **Classifier database** — imports token counts from existing Python pickle/dbm databases
- **Message database** — loads per-message classification history

## Compatibility

- **Outlook**: 2016, 2019, 2021, Microsoft 365 (32-bit and 64-bit)
- **Windows**: 10, 11
- **Account types**: Exchange, Outlook.com/Hotmail, IMAP (Hotmail uses Outlook Object Model fallback for folder resolution)

## Author

**cyberblob/Doug Farrell** — Complete ground-up rewrite in Rust. No code from the original Python SpamBayes project is present in this implementation.

## License

MIT License

Copyright (c) 2026 cyberblob

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.

---

*This project implements the same Bayesian spam classification algorithm described in Paul Graham's "A Plan for Spam" (2002) and the subsequent SpamBayes research. The implementation is entirely original Rust code with no source derived from the Python SpamBayes project.*
