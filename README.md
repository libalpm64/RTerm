# RTerm V2
Rterm is a lightweight terminal emulator built with Rust and egui. I built this because Windterm kept crashing whenever I tried to load SSH keys. I also wanted a terminal without a bloated frontend or TypeScript. Rterm now uses a native Rust UI with the Alacritty terminal engine.

<img width="2048" height="1106" alt="image" src="https://github.com/user-attachments/assets/3ef9b1cc-c68c-4c14-bcbc-41626700579a" />

## Features
* Local terminal sessions
* SSH sessions (Russh-based async engine)
* Telnet sessions
* Serial sessions (experimental)
* Automatic SSH reconnect and host key verification
* Session list with filtering and search
* Tab management and resizable side panels
* SSH key manager with key generation and import
* SSH agent support
* Vault encryption for stored sessions and keys (XChaCha20-Poly1305)
* Automatic lock screen
* Local and remote directory browsing
* File actions and bulk selection
* SFTP upload and download for files and folders
* Concurrent SFTP downloads and single-file transfer recovery
* Transfer progress and remote file permissions
* Independent bottom shell
* Terminal zoom and fullscreen mode
* Command history and shell integration (OSC 133)
* Syntax-style highlighting for shell and network text
* Configurable cursor and terminal line numbers
* Custom background images with blur and opacity
* Unicode input and clipboard support
* Native Rust UI with no webview
* Optional AI agent with command execution and SFTP uploads
* AI activity history and permission controls
* OpenAI Secure MCP Tunnel support for ChatGPT

## Languages
* English
* Simplified Chinese (简体中文)
* Japanese (日本語)
* Spanish (Español)
* Brazilian Portuguese (Português)
* Russian (Русский)
* German (Deutsch)
* Korean (한국어)
* French (Français)
* Traditional Chinese (繁體中文)
* Italian (Italiano)
* Polish (Polski)
* Turkish (Türkçe)
* Ukrainian (Українська)

Rterm follows your system language by default. You can change it in Settings > Appearance > Language. Chinese and Japanese and Korean may require installed system fonts.

Translations are AI-generated. If you speak one of these languages please help us fix mistakes!

## Targets
Supported platforms:
* Windows (x64)
* macOS (Apple Silicon and Intel)
* Linux (x64)

## Installation
* Windows: Setup EXE
* macOS: PKG
* Linux: DEB or portable tar.gz

Linux packages target Arch, Debian or compatible distributions. See [build and installation instructions](docs/USAGE.md#installation).

## AI setup
AI is optional and disabled by default. You can use the local rterm-agent CLI or connect ChatGPT through a private MCP tunnel.

* [OpenAI Secure MCP Tunnel setup](docs/AI.md#openai-secure-mcp-tunnel-for-chatgpt)
* [Official OpenAI tunnel guide](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels)
* [Local AI agent setup](docs/AI.md#ai-agent-interface-local-cli)

## Build from source
Install stable Rust and your platform's C toolchain.

```sh
cargo build --release --locked --bins
cargo run --locked
```

See [usage and development](docs/USAGE.md) for more details.
