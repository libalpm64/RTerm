# Usage and development

## Installation

The repository includes native installer recipes and a **Build installers** GitHub Actions workflow. It builds both matching binaries from the same commit.

| Platform | Package | Default installation |
|---|---|---|
| Windows x64 | Setup EXE | Per-user Programs/RTerm; adds the installation folder to user PATH and provides a Start menu shortcut |
| macOS (Apple silicon or Intel) | PKG | /Applications/RTerm.app; CLI symlink in /usr/local/bin |
| Linux x64 | DEB | /usr/bin and desktop application menu |
| Linux x64 | tar.gz | Extract and run sh install.sh; defaults to ~/.local/bin (or /usr/local/bin as root; optional PREFIX) |

Linux packages are built on Ubuntu 24.04 and require compatible glibc and graphics libraries. They are not universal across every Linux distribution. Every RTerm installer bundles the same latest stable, checksum-verified official OpenAI tunnel-client for its target architecture, with upstream license notices. The client is installed beside RTerm on Windows and in the macOS app bundle, or in the private lib/rterm directory on Linux. No separate network download or manual client selection is needed at install time. Hosted CI produces ad-hoc signed macOS and unsigned Windows test packages. See [release builds](#release-builds) for trusted distribution.

Desktop shortcuts are optional. The Windows installer offers a **Create a desktop shortcut** checkbox (unchecked by default). The Linux portable installer asks when run interactively as a normal user. The macOS PKG and Linux DEB offer a one-time shortcut choice on first launch as the actual desktop user, rather than creating shortcuts in the installing administrator's home. The macOS shortcut links to the installed app; Linux creates an executable desktop launcher in the user's configured desktop directory, if available. Some Linux desktops require manually allowing launch of desktop files. Existing shortcuts are not overwritten by the Unix setup, and declining the offer leaves the desktop unchanged.

Application menu registration is automatic, separate from desktop shortcuts: Windows adds a Start menu entry, macOS installs a Utilities-category app bundle discoverable in Applications and system app search, and Linux registers an XDG desktop entry for GNOME, KDE Plasma and other compatible launchers. The portable Linux installer registers the entry in the user's XDG data directory (or the system local applications directory for elevated installs), with an absolute executable path even when a custom PREFIX is used. Search for **RTerm**, **Terminal**, or **SSH** in the Linux application launcher. Linux registers the bundled RTerm icon with the desktop theme. Dock, taskbar and favorites pinning remain user choices.

The shared command is `rterm-agent --version`. Restart your terminal or AI client after installing so it sees PATH changes. On macOS ensure /usr/local/bin is on PATH; on Linux portable installs ensure ~/.local/bin (or /usr/local/bin for system installs, or PREFIX/bin) is on PATH. When installing as root, choose a trusted system-owned prefix accessible to desktop users. Development builds can invoke the companion in target/debug or target/release directly; no checkout or username is embedded in the copied prompt.

Maintainers: **Build installers** runs on pushes to main and can also be started manually. Releases from main publish only after the audit and every native installer job succeed. Each release is named RTerm # followed by the short commit ID. Its tag uses the full commit ID. Enable release immutability in GitHub repository Settings > General > Releases before publishing. The workflow uploads all installers and verified checksums to a draft first and checks the immutable flag after publication. Existing releases are never overwritten.

## Build and run

Install stable Rust and the platform’s C toolchain, then run these commands from the repository root.

```sh
cargo run --locked
cargo build --release --locked
cargo test --locked
```

- **Windows** uses ConPTY for local terminal sessions. Use Windows 10 version 1809 or later and the Visual Studio C++ Build Tools with the default MSVC Rust toolchain.
- **macOS** uses a Unix PTY. Install the Xcode command-line tools with `xcode-select --install`.
- **Linux** uses a Unix PTY and works with X11 or Wayland. A desktop environment with OpenGL/EGL and an XDG desktop portal provides rendering and native file pickers.

On Windows x64, if Cargo reports that `link.exe` is missing, you can use MinGW-w64 GCC instead. With GCC on your PATH, run these commands from the Rterm checkout to select GNU for this directory.

```cmd
rustup toolchain install stable-x86_64-pc-windows-gnu
rustup override set stable-x86_64-pc-windows-gnu
cargo run --locked
```

Release binaries are written to `target/release/rterm` on macOS and Linux or `target/release/rterm.exe` on Windows. `./run.sh` also launches the app on macOS and Linux.

## Languages

Choose the interface language in **File → Settings → Appearance → Language**, then select Apply. System default follows the operating system's preferred language where supported. Missing translations and unsupported system languages fall back to English.

The 14 catalogs cover English, Simplified Chinese, Japanese, Spanish, Brazilian Portuguese, Russian, German, Korean, French, Traditional Chinese, Italian, Polish, Turkish and Ukrainian. The selector shows each language in its own script. Hindi, Hebrew and Arabic are deferred until text shaping and bidirectional rendering are adequate.

Chinese, Japanese and Korean require compatible installed system fonts. Install the appropriate OS language/font pack, or Fontconfig and Noto Sans CJK on Linux (both are DEB dependencies; portable installs use installed system fonts). Linux discovers fonts via Fontconfig, including Fedora variable fonts and user-installed fonts. RTerm loads installed CJK fonts for the language selector and terminal independently of the interface language. If RTerm cannot load a recognized font for the selected script, it falls back to English; restart after installing the font. OS fonts are not redistributed.

Translations are AI-generated and corrections are welcome. Edit the UTF-8 JSON catalogs in [locales](../locales/). Keep English source keys and placeholders unchanged. Preserve technical terms and run `cargo test --locked --bin rterm i18n::tests`. Terminal output and diagnostic errors are not translated. Some advanced controls may still appear in English.

## Release builds

Run the Native builds and Build installers workflows. Review the dependency audit exceptions in .cargo/audit.toml at each release. Test the installers on each supported platform before publishing. Confirm the first-party license and artwork rights before distributing the application. Review third-party notices and never include local credentials or vaults.

Packaging requires Bash, curl, jq, unzip, and SHA-256 utilities (sha256sum or shasum). Windows uses Git Bash from Git for Windows; jq must be available on PATH. Each commit-triggered installer workflow resolves the latest stable official OpenAI tunnel-client release once and shares its version and published SHA-256 digests across all platform jobs. Local packaging resolves latest by default too. Missing assets or hashes, or a checksum mismatch, fail the build instead of silently using an older version. For an offline repeat build, first save the release snapshot with `bash packaging/fetch_tunnel_client.sh --resolve manifest.json`, then set `RTERM_TUNNEL_MANIFEST` to that snapshot and `RTERM_TUNNEL_ARCHIVE` to its matching upstream ZIP.

Generate the license report required by the packaging scripts:

```sh
cargo install cargo-about --version 0.9.3 --features cli --locked
mkdir -p target/release-notices
cargo about generate --locked --fail -o target/release-notices/THIRD-PARTY-LICENSES.html packaging/licenses.hbs
```

On Windows use `New-Item -ItemType Directory -Force target/release-notices` instead of mkdir.

* Windows: install Inno Setup 6 and Windows SDK SignTool. Set RTERM_WIN_CERT_THUMBPRINT to the installed signing certificate thumbprint and run `powershell -File packaging/windows/package.ps1 -Sign`. Verify the installer and embedded uninstaller signatures.
* macOS: provision Developer ID Application and Installer identities and a notarytool keychain profile. Set RTERM_SIGN=1 and RTERM_MAC_APP_IDENTITY and RTERM_MAC_INSTALLER_IDENTITY and RTERM_MAC_NOTARY_PROFILE. Run `sh packaging/macos/package.sh target/release 2.0.0 arm64 dist` with the appropriate version and architecture. Verify signing and notarization.
* Linux: use the packaging scripts and run `bash packaging/linux/test_install.sh` as an unprivileged user with gio installed. Run the tunnel tests with bash packaging/test_fetch_tunnel_client.sh (requires jq and zip). Run ShellCheck on the shell scripts.

Verify installs and upgrades and uninstalls plus local terminal and SSH and SFTP and vault and language and agent permission behavior. The legacy vault's shared derivation salt still needs a future migration to per-vault random salts. Publish only artifacts built from the intended committed revision with matching SHA-256 checksums.

## SSH host verification

Interactive SSH requests a UTF-8 PTY and the character locale LC_CTYPE=C.UTF-8. The server may decline locale requests or override them in its shell startup files. If non-ASCII input still fails, run `locale charmap` in the remote shell; it should report UTF-8. On Linux, check `locale -a` for an installed UTF-8 locale and configure your remote shell to use it. This does not translate terminal data or change server configuration.

On first connection, compare the displayed SHA-256 fingerprint with the server's fingerprint from a trusted source. **Trust and Connect** remembers the key for that host and port. RTerm checks the key before sending authentication credentials on each connection, including automatic reconnects. A changed key stops the connection and displays both fingerprints. Use **Trust Replacement and Connect** after verifying a planned rotation. **SSH Keys → Known Hosts…** lists trusted fingerprints and lets you forget a key.

## Terminal command navigation

**Session → Previous Command** and **Next Command** jump to OSC 133 shell markers. The shortcuts are **Alt+Shift+Up** and **Alt+Shift+Down**. Command markers show a duration and a success or failure glyph in the line-number gutter. Hover over a marker for the exit code. AI Activity reports the elapsed time from the isolated execution channel and its SSH exit status.

Enable shell integration in the interactive shell where you run commands. RTerm understands OSC 133 markers from existing integration scripts. For Bash, these hooks can be added to the relevant `.bashrc`. Start a new interactive shell after adding them.

```bash
if [[ $- == *i* && ${__rterm_marks_enabled:-0} != 1 ]]; then
    __rterm_marks_enabled=1
    __rterm_before_prompt() {
        local status=$?
        printf '\033]133;D;%d\007\033]133;A\007' "$status"
        return "$status"
    }
    PROMPT_COMMAND=(__rterm_before_prompt "${PROMPT_COMMAND[@]}")
    PS0=$'\033]133;C\007'"${PS0:-}"
fi
```

RTerm batches draw updates for applications using DEC mode 2026 and flushes a buffered frame if the application leaves the mode active past its timeout.

## SFTP transfer recovery

With **Settings → SFTP → Resume Transfers** on, repeat an interrupted single-file transfer with the same source and destination after reconnecting. RTerm saves checkpoints and keeps the partial file separate until completion. Parallel downloads keep their completed byte ranges and check remote metadata and sampled bytes. Uploads check the local file's hash and sample the staged remote bytes before resuming. RTerm starts a fresh transfer if those checks show a change.

An upload is published using the server's rename operation. If the server refuses to replace an existing target, the completed partial remains available for retry after you resolve the destination conflict. Directory downloads require a new destination directory rather than merging with an existing directory. Directory transfers and AI-approved SFTP uploads do not use single-file transfer recovery.

## Terminal benchmarks

Run the opt-in SFTP listing latency benchmark with `cargo test --release --locked --features benchmark --bin rterm listing_latency_benchmark -- --ignored --nocapture`. The normal test suite separately checks directory listing correctness without artificial network delays.

The benchmark opens a native window and runs the workload in a real local or SSH session. Save a case as `target/benchmark.json` and run `cargo run --release --locked --features benchmark -- --benchmark target/benchmark.json`.

```json
{
  "transport": { "kind": "local", "shell": "/bin/bash" },
  "workload": { "kind": "sequence", "lines": 1000000 },
  "scrollback": 100000,
  "warmups": 1,
  "runs": 3,
  "report": "target/benchmark-report.json"
}
```

- On Windows, point `shell` to a Bash installation that provides `seq` and `cat`.
- For SSH, use `{"kind":"ssh","host":"test-server","user":"tester","key":"/path/to/test-key"}` as the transport. The harness uses port 22 and disables SSH compression.
- To benchmark an existing SSH profile without copying credentials into a JSON file, use `{"kind":"saved","name":"My server"}` as the transport. Unlock the vault in the benchmark window if prompted. The saved-profile benchmark uses your normal terminal appearance settings, overrides the requested scrollback in memory, and does not save the profile or benchmark command history.
- To measure interactive command response, use `{"kind":"latency","preload_lines":100000}` as the workload. The optional preload streams numbered lines into the terminal before timing repeated shell completion markers. The reported samples are command submission to parsed terminal response, not physical keyboard-to-display latency; `payload_bytes` is zero because this is a latency test rather than a throughput test.
- For bulk text, use `{"kind":"text","path":"/path/to/corpus.b64","bytes":102401504}` as the workload. Place the file on the machine that runs the shell and set `bytes` to its exact size. [WindTerm's corpus recipe](https://github.com/kingToolbox/WindTerm/blob/master/benchmark/urandom_test.sh) describes the reference workload.

Each reported time runs from command submission to parsing its completion marker in the terminal grid. Compute MiB/s by dividing `payload_bytes` by 1,048,576 and by the median of `seconds`. On Windows, the report includes RTerm's working set and private bytes just before the first run and the peak values sampled during the workloads, including warmups. Keep the window size, scrollback, and workload consistent when comparing runs.

To profile the CPU cost of scrolling a full 100,000-line history, run `cargo test --release --locked --features benchmark scrollback_frame_profile -- --ignored --nocapture`. It reports median and p95 frame times (including text tessellation but excluding GPU presentation) and shape count over 60 measured frames with semantic highlighting enabled.

To isolate the cost of the window and renderer from the application and shell, use `cargo run --release --locked --features benchmark -- --benchmark-idle renderer target/renderer.json`. Replace `renderer` with `workspace` (no session) or `local` (default local shell). An optional final argument sets the settle time in seconds. The Windows reports include working set and private committed memory before creating the window and after settling; neither is the same as Task Manager's active private working set.

## Code layout

- `src/app/` holds the desktop layout and session sidebar. `src/dialogs/` holds the connection, settings, and key forms.
- `src/terminal.rs` handles PTY input and output, keyboard input, selection, and terminal rendering.
- `src/backend.rs` routes typed commands. `src/backend/connection.rs` handles address normalization, authentication, and terminal transports. `src/backend/sftp.rs` manages remote files and transfers.
- `src/storage.rs` handles settings, saved profiles, keys, and vault compatibility.

The bundled fonts are distributed under their respective SIL Open Font Licenses in `assets/`.
