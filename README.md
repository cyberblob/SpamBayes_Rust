# SpamBayes Outlook Add-in (Rust)

A native Rust implementation of the SpamBayes Outlook COM add-in. This is a ground-up rewrite of the original Python SpamBayes Outlook2000 plugin, compiled to a standalone 64-bit DLL that loads directly into Microsoft Outlook without requiring a Python runtime.

**Version:** 0.3.0-alpha.3

## Overview

SpamBayes uses a Bayesian statistical classifier to sort email into ham (good), spam, or unsure. This Rust version provides:

- **Native 64-bit COM add-in** — loads as an in-process DLL, no Python or runtime dependencies
- **Real-time folder monitoring** — watches configured folders via Outlook's `Items.ItemAdd` COM events
- **Timer-based batching** — configurable start delay + interval to batch rapid message arrivals
- **Automatic scoring and filtering** — classifies messages and moves them to configured spam/unsure folders
- **Bounce-back detection** — detects and re-moves pre-scored messages that return to watched folders
- **Calendar spam filtering** — detects meeting/appointment spam with Prompt/Trash/Move actions
- **Ribbon UI** — Spam, Not Spam, Show Clues, and Manager buttons in Outlook's ribbon
- **GTK4 Manager GUI** — standalone configuration and training interface (`spambayes_manager.exe`)
- **Show Clues viewer** — displays token evidence for a selected message's score (`spambayes_clues.exe`)
- **Notification sounds** — WAV playback with priority logic (Ham > Unsure > Spam)
- **Session and lifetime statistics** — counters persisted to JSON across restarts
- **Configuration wizard** — 6-page first-run setup for folder selection and thresholds
- **Training data export** — export to Ham/Spam bucket directories
- **Spam auto-cleanup** — delete old spam based on configurable retention period
- **Configuration migration** — reads existing Python SpamBayes INI files and classifier database
- **Zero-dependency installer** — InnoSetup-based, single-file setup

## Architecture

The project is organized as a Cargo workspace with five crates:

```
Outlook365/
├── Cargo.toml                  # Workspace manifest (5 member crates)
├── Cargo.lock                  # Locked dependency versions
├── build_all.bat               # Build + deploy + installer script
├── .cargo/config.toml          # Static CRT linking, default x86_64 target
│
├── crates/
│   ├── spambayes-core/         # Classifier + tokenizer (pure Rust, no Windows deps)
│   ├── spambayes-config/       # INI parsing, config chain, migration, profiles
│   ├── spambayes-storage/      # Database persistence (mmap-backed dbm)
│   ├── spambayes-mapi/         # MAPI session, message store, folder access
│   └── spambayes-addin/        # COM DLL + Manager GUI + Clues viewer
│
├── gtk4-bundle/                # GTK4 runtime DLLs bundled for installer
├── installer/
│   ├── spambayes_outlook.iss   # InnoSetup script (64-bit only)
│   └── output/                 # Built installer EXE
├── scripts/                    # Build helper scripts (GTK4 bundling, etc.)
└── tests/                      # Integration tests
```

### Crate Dependency Hierarchy

```
spambayes-core       (pure Bayesian logic, no Windows deps)
    ↑
spambayes-config     (INI config, folder IDs, profiles, migration)
    ↑
spambayes-storage    (mmap dbm, pickle import, message DB)
    ↑
spambayes-mapi       (MAPI session, store, folder, message)
    ↑
spambayes-addin      (COM DLL — the final artifact Outlook loads)
```

### Crate Responsibilities

| Crate | Purpose |
|-------|---------|
| `spambayes-core` | Bayesian classifier (Robinson-method, chi-squared combining), RFC 2822 MIME tokenizer. Platform-independent. |
| `spambayes-config` | INI file parsing, `AppConfig` structs, `FolderId` types, layered config chain, Python config migration. |
| `spambayes-storage` | Mmap-backed token database, message metadata DB, Python pickle import, atomic file writes. |
| `spambayes-mapi` | Windows MAPI session management, message store operations, folder enumeration, retry logic. |
| `spambayes-addin` | COM add-in entry point, ribbon UI, toolbar, folder event sinks, filter engine, training engine, timer state machine, notification manager, statistics, GTK4 Manager GUI, Show Clues viewer. |

## Building

### Prerequisites

- Rust toolchain (stable, latest) with the 64-bit MSVC target:
  ```
  rustup target add x86_64-pc-windows-msvc
  ```
- Visual Studio Build Tools 2019+ (MSVC linker + Windows SDK)
- MSYS2 with GTK4 dev libraries (ucrt64 — for Manager GUI)
- InnoSetup 6 (for installer, optional)

### Build Commands

```cmd
# From the Outlook365/ directory:

# Full pipeline: build DLL + Manager + Clues, deploy, bundle GTK4, build installer
build_all.bat

# Build without deploying to install directory
build_all.bat --no-deploy

# Build DLL only (no GUI features)
cargo build --release --target x86_64-pc-windows-msvc --lib

# Build Manager GUI + Clues viewer
cargo build --release --target x86_64-pc-windows-msvc --bin spambayes_manager --bin spambayes_clues --features gui

# Run unit tests
cargo test --target x86_64-pc-windows-msvc

# Build installer (requires Inno Setup 6)
"C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\spambayes_outlook.iss
```

### Build Output

```
target/x86_64-pc-windows-msvc/release/spambayes_addin.dll     # COM DLL (loaded by Outlook)
target/x86_64-pc-windows-msvc/release/spambayes_manager.exe   # GTK4 Manager GUI
target/x86_64-pc-windows-msvc/release/spambayes_clues.exe     # Show Clues viewer
installer/output/SpamBayes_Outlook_Setup_0.3.0a3.exe           # Installer
```

### Key Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `windows` | 0.58 | Win32 API bindings (COM, Registry, MAPI, UI) |
| `thiserror` | 1 | Error type derivation |
| `mailparse` | 0.15 | Email/MIME parsing for tokenizer |
| `memmap2` | 0.9 | Memory-mapped file I/O for storage |
| `indexmap` | 2 | Ordered hash maps for config |
| `gtk4` | 0.9 | GTK4 Rust bindings (Manager GUI, feature-gated) |
| `serde` + `serde_json` | 1 | JSON serialization for statistics |
| `ureq` | 2 | HTTP client for version checking |
| `crossbeam-channel` | 0.5 | Thread-safe message passing |
| `proptest` | 1 | Property-based testing (dev only) |
| `criterion` | 0.5 | Benchmarking (dev only) |

## Installation

### Via Installer (Recommended)

Run `SpamBayes_Outlook_Setup_0.3.0a3.exe` as Administrator. The installer will:
1. Install the DLL, Manager, and Clues viewer to `C:\Program Files\SpamBayes\`
2. Bundle GTK4 runtime DLLs alongside the executables
3. Register the COM add-in via `regsvr32` (64-bit)
4. Create Start Menu shortcuts for the Manager
5. Outlook will load SpamBayes on next startup

The installer requires 64-bit Outlook and Windows 10 or later.

### Manual Registration

```cmd
regsvr32 "C:\Program Files\SpamBayes\spambayes_addin.dll"
```

To unregister:
```cmd
regsvr32 /u "C:\Program Files\SpamBayes\spambayes_addin.dll"
```

## Configuration

Configuration uses a layered INI system stored in `%LOCALAPPDATA%\SpamBayes\`:

| File | Purpose |
|------|---------|
| `global.ini` | Global settings (shared across profiles) |
| `default.ini` | Profile-specific settings |
| `spambayes.db` | Classifier token database |
| `spambayes_msg.db` | Per-message metadata |
| `spambayes_stats.json` | Lifetime statistics |
| `addin_debug.log` | Debug log |

The config chain applies settings in order: **defaults -> global.ini -> profile.ini** (sparse save — only non-default values are written).

### Key Settings

| Section | Key | Description |
|---------|-----|-------------|
| `[Filter]` | `enabled` | Enable/disable real-time filtering |
| `[Filter]` | `watch_folder_ids` | Folders to monitor for new messages |
| `[Filter]` | `spam_folder_id` | Destination for classified spam |
| `[Filter]` | `unsure_folder_id` | Destination for unsure messages |
| `[Filter]` | `spam_threshold` | Score at/above which = spam (default: 90%) |
| `[Filter]` | `unsure_threshold` | Score at/above which = unsure (default: 15%) |
| `[Filter]` | `timer_enabled` | Background timer-based filtering |
| `[Filter]` | `timer_start_delay` | Seconds before first timer fires |
| `[Filter]` | `timer_interval` | Seconds between timer ticks |
| `[General]` | `cleanup_enabled` | Auto-delete old spam |
| `[General]` | `cleanup_days` | Retention period for spam folder |
| `[Calendar]` | `calendar_action` | Action for calendar spam (Prompt/Trash/Move) |
| `[Notification]` | `sound_enabled` | Enable notification sounds |

The Manager GUI (launched from the ribbon or Start Menu) provides a visual interface for all settings.

## How It Works

### Startup Sequence

1. Outlook loads `spambayes_addin.dll` via COM (`DllGetClassObject` -> `IClassFactory` -> `AddinCore`)
2. `OnConnection` — stores Application pointer, initializes logger, MAPI session, loads config chain and classifier database
3. `OnStartupComplete` — defers UI and folder hook setup via Windows timers (avoids COM reentrancy)
4. Toolbar timer (1.5s) — creates CommandBar buttons via Outlook Object Model
5. Folder hook timer (2.5s) — resolves watch folders, connects `ItemAdd` event sinks
6. First-run detection — if no config file exists, launches the configuration wizard

### Message Processing Flow

```
New message arrives in watched folder
        |
        v
ItemAdd event fires on Items collection
        |
        v
Timer batching (start delay + interval processing)
        |
        v
Extract message content (MIME or Headers+Body via PropertyAccessor)
        |
        v
Tokenize (headers, body, HTML, URLs, skip-bigrams)
        |
        v
Score with Bayesian classifier (Robinson-method, chi-squared combining)
        |
        v
Classify: Score >= spam_threshold -> Spam
          Score >= unsure_threshold -> Unsure
          Otherwise -> Ham
        |
        v
Perform action: Move to spam/unsure folder, update statistics, play notification
```

### Bounce-Back Handling

When watching Exchange-managed folders like "Junk Email", Exchange may move messages back after SpamBayes moves them. The add-in detects these bounced messages by checking for existing score metadata and re-moves them to the correct destination without re-scoring.

### COM Registration

The DLL registers under:
- **CLSID**: `{A3B9E8D1-4F2C-4A6E-B8D7-1234567890AB}`
- **ProgID**: `SpamBayes.OutlookAddin`
- **Outlook Addins**: `HKCU\Software\Microsoft\Office\Outlook\Addins\SpamBayes.OutlookAddin`
- **LoadBehavior**: 3 (load at startup)
- **Threading Model**: Apartment (STA)

## User Interface

### Ribbon Buttons
- **Spam** — mark selected message(s) as spam, train classifier, move to spam folder
- **Not Spam** — mark selected message(s) as ham, train classifier, move back to inbox
- **Show Clues** — launch Clues viewer showing token evidence for the selected message's score
- **Manager** — launch the GTK4 Manager GUI for configuration and training

### Context Menu
- Right-click any message for Spam/Not Spam options

### Manager GUI (spambayes_manager.exe)
- View session and lifetime statistics
- Configure thresholds, folders, and notification settings
- Browse MAPI folder tree for folder selection
- Trigger batch training from configured ham/spam folders
- View classifier database health (token counts, training totals)

## Logging

The add-in writes timestamped diagnostic logs to `%LOCALAPPDATA%\SpamBayes\`:

| Log File | Content |
|----------|---------|
| `addin_debug.log` | COM lifecycle, toolbar, timer events, configuration, errors |

Example log output:
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
- **Static CRT linking**: No runtime DLL dependencies (self-contained binary)
- **Memory safety**: Rust ownership model prevents buffer overflows and use-after-free
- **COM safety**: Unsafe blocks are confined to COM vtable implementations with documented invariants
- **No network access**: The add-in processes email locally; no data is sent externally

## Migration from Python SpamBayes

The Rust add-in automatically detects and migrates from the Python version:
- **Configuration** — reads Python-format INI from `%APPDATA%\SpamBayes\`, converts to Rust format in `%LOCALAPPDATA%\SpamBayes\`
- **Classifier database** — imports token counts from existing Python pickle/dbm databases
- **Message database** — loads per-message classification history

## Compatibility

- **Outlook**: Microsoft 365, 2021, 2019, 2016 (64-bit only)
- **Windows**: 10, 11 (64-bit)
- **Account types**: Exchange, Outlook.com/Hotmail, IMAP

## Project Status

This is an active development project (alpha). Current stats:
- 5 workspace crates, 44 source files
- 400+ unit tests across all crates
- 53 implemented features (see `Outlook365/docs/RUST_ADDIN_STATS.md`)

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
