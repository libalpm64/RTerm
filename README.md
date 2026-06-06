# Rterm
Rterm is a lightweight, experimental terminal emulator built with Rust, Wry, and xterm.js. I built this because Windterm kept crashing whenever I tried to load SSH keys, and I grew tired of open-source “DevOps” terminals being cluttered with AI bloat and TypeScript.

<img width="800" height="600" alt="image" src="https://github.com/user-attachments/assets/835a7ad0-e4f2-4ca1-a7c7-5f19666f16e2" />

## Features
* Local terminal sessions
* SSH sessions (Russh-based async engine)
* Telnet sessions
* Serial sessions (experimental)
* SFTP subsystem integration
* Session list with filtering/search
* Tab management
* SSH key manager UI
* Vault encryption for stored sessions/keys (ChaCha)
* Automatic Lock screen
* SSH agents
* Directory browsing
* Context menu actions: delete, move, rename, info, select mode
* Bulk select mode with bulk delete/move
* Remote directory browsing
* Upload and download through SFTP, RSYNC (coming soon)
* Context menu actions: download, move, rename, info, select mode
* Bulk selection
* Remote permission display in info dialogs
* Concurrent/parallel SFTP download paths
* Transfer progress overlays
* xterm.js integration
* Zoom in/out
* Command history support
* Syntax/AST-style highlighting for shell/network text patterns
* Rust + Wry + xterm.js desktop stack
* Lean frontend (vanilla JS, no TypeScript toolchain)
* Explicit IPC method surface for frontend/backend features
* Open source codebase with modular JS/Rust components

## Targets
Supported Platforms: macOS, Linux.
