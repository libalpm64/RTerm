# Rterm

Rterm is a lightweight, experimental terminal emulator built with Rust, Wry, and xterm.js. I built this because Windterm kept crashing whenever I tried to load SSH keys, and I grew tired of open-source “DevOps” terminals being cluttered with AI bloat and TypeScript hell. It is fully open-source and focuses on features that actually work, even if nobody asked for them.

* SSH Supported
* SSH Keys / SSH Agent
* Telnet
* SFTP
* Serial (Untested)
* Vault (SSH keys and passwords with ChaCha)
* Fork of the Russh engine (Async SSH non-blocking)
* Modularity (IPC handlers)
* Lean (No NPM packages, No Typescript, no bs)
* Fully open source
* 50 MB Runtime (That’s Apple Webkit overhead)
* Concurrent SFTP transfers
* Customizable Line Rate
* Local explorer
* File Downloading
* File uploading (Experimental)
* Filter sessions
* AST parsing (Bash, Linux, IPs, domains)

Why Rterm?
This project started after repeated crashes in Windterm when loading SSH keys. Instead of waiting for a billion years to fix it, you can fix any bug in literal minutes and add as many features as you want on top of it in like a day.
RTerm was built in like a week using Deepseek, because I had some credits from them (the cost was $5 for everything in Rterm).

I do not guarantee anything. This is a personal project. I just unprivated it because some people might actually use this.

Supported Platforms: macOS

Linux support coming soon. I’m too lazy to switch PCs.
