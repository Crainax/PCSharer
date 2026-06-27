# PC Sharer

LAN file sharing desktop app for Windows/macOS, built with Tauri 2, React, and Rust.

## Version 1 scope

- Discover other running PC Sharer devices on the same LAN by UDP broadcast.
- Add a target manually by IP when broadcast discovery is blocked.
- Drag files or folders into the app, or select them with a dialog.
- Send files directly over TCP from disk to socket to disk.
- Receive automatically into `Downloads/PCSharer` unless changed in the app.
- Show transfer progress and recent transfer records.

## Development

```powershell
npm install
npm run tauri:dev
```

## Build Windows exe

```powershell
npm run release
```

The portable exe is generated at:

```text
src-tauri/target/release/pc-sharer.exe
```

Windows may show a firewall prompt on first launch. Allow private-network access on both computers for LAN discovery and transfer.
