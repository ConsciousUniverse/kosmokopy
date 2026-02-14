# Kosmokopy

A GTK4 file copier and mover with filtering, integrity verification, and SSH remote transfer support.

![Rust](https://img.shields.io/badge/Rust-2021-orange) ![GTK4](https://img.shields.io/badge/GTK4-0.9-blue) ![License](https://img.shields.io/badge/license-GPLv3-blue)

## Features

### Source Selection
- **Browse Folder** — select a directory and recursively process all files within it
- **Browse Files** — pick individual files for transfer

### Transfer Modes
- **Copy** — duplicate files to the destination
- **Move** — transfer files to the destination and remove the originals
- **Files Only** — flatten all files into the destination directory (no subdirectories)
- **Folders and Files** — preserve the original directory structure at the destination

### Exclusions
- **Exclude Directories** — pick directories to skip (all contents are excluded recursively)
- **Exclude Files** — pick individual filenames to skip wherever they appear
- **Clear** — remove all exclusion rules
- Exclusions are displayed in a read-only scrollable list

### Overwrite Handling
Kosmokopy compares source and destination files byte-by-byte before deciding what to do:

| Destination file | Content | Copy mode | Move mode |
|---|---|---|---|
| Doesn't exist | — | Copy normally | Move normally |
| Exists, identical | Same bytes | Skip ("identical at destination") | Delete source only (no transfer needed) |
| Exists, different | Different + overwrite **on** | Overwrite with new version | Overwrite, then delete source |
| Exists, different | Different + overwrite **off** | Skip ("different version exists") | Skip |

### Integrity Verification
- Every file copy is verified byte-by-byte against the source
- If verification fails on copy, the bad copy is removed
- If verification fails on move, the original is retained
- Same-filesystem moves use `rename()` (instant pointer change, no data copied)

### SSH Remote Transfers
Transfer files to remote machines using SSH config hosts:
- Type `hostname:/remote/path` in the destination field (e.g. `ubuntu:/home/dan/backup`)
- The hostname must match an entry in `~/.ssh/config`
- Uses SSH connection multiplexing for performance
- Creates remote directories automatically
- Remote overwrite detection checks existing files before transfer
- For moves, local files are deleted after successful transfer
- Integrity is guaranteed by the SSH protocol

### Progress and Reporting
- Real-time progress bar showing file count and current filename
- Completion dialog with summary of copied, skipped, and excluded files
- Detailed skip reasons (identical, already exists, different version)
- Scrollable error list if any transfers fail

## Requirements

### Build Dependencies
- Rust toolchain (edition 2021, Cargo 1.70+)
- GTK4 development libraries

#### macOS
```bash
brew install gtk4
```

#### Ubuntu / Debian
```bash
sudo apt install libgtk-4-dev build-essential
```

### Runtime Dependencies
- GTK4 runtime libraries
- `ssh` and `scp` (only for remote transfers — present on any system with SSH configured)

## Building

### From Source
```bash
cargo build --release
```

The binary is at `target/release/kosmokopy`.

### macOS (.dmg)
```bash
./macos/build-dmg.sh
```

Creates `target/macos/Kosmokopy-0.1.0-arm64.dmg` containing a drag-to-install `.app` bundle.

> **Note:** GTK4 must be installed via Homebrew on the target Mac.

### Linux (AppImage)
```bash
./appimage/build-appimage.sh
```

Creates a portable `target/appimage/Kosmokopy-0.1.0-x86_64.AppImage`.

> **Note:** GTK4 runtime libraries must be installed on the target system.

## Usage

1. **Select source** — click "Browse Folder" for a directory or "Browse Files" for individual files
2. **Set destination** — browse for a local folder, type a path, or enter `host:/path` for remote
3. **Choose mode** — Copy or Move, Files Only or Folders and Files
4. **Set exclusions** (optional) — use the Exclude Directories / Exclude Files buttons
5. **Toggle overwrite** (optional) — check "Overwrite existing files" to replace differing files
6. **Click Transfer**

## Author

**Dan Bright** — [dan@danbright.uk](mailto:dan@danbright.uk)

This code was primarily authored using artificial intelligence (Claude Opus 4.6 model).

## License

Copyright (C) 2026 Dan Bright

This project is licensed under the **GNU General Public License v3.0** — see [LICENSE](LICENSE) for details.

All third-party dependency licenses (MIT, Apache-2.0, Unlicense) are bundled in [THIRD-PARTY-LICENSES.txt](THIRD-PARTY-LICENSES.txt).
