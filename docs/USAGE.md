# Usage and development

## Import sessions

In RTerm, open **File → Settings → Security**, enable the vault, set a master password, and click **Apply**. With the vault unlocked, open **Settings → Import → Import Sessions…** and select an export. RTerm detects the format from the file contents. The importer currently supports the CSV export from Netcatty’s **Hosts** view and suggests the most recent Netcatty download when it finds one. This workflow supports macOS, Windows, and Linux.

RTerm adds direct SSH and Telnet profiles to the encrypted vault and preserves existing duplicates. SSH passwords, key-file references, and passphrases included in the CSV are imported. For file-based keys, the key file must be available at the saved path on the receiving machine. Netcatty’s CSV can contain plaintext credentials. Secure Enclave keys remain device-bound and can be used through RTerm’s SSH agent option if an agent exposes them. Groups, tags, and Telnet login credentials are omitted. Rows with a proxy or jump host in the export, unsupported protocols, or invalid fields are counted as skipped.

## Vault backup and restore

Open **Settings → Security → Encrypted vault backups** on Windows, Linux or macOS. Apply any pending settings before starting a backup.

- **Export vault:** Unlock the vault, select Export vault and choose a new .rterm-vault filename. The file is an exact encrypted copy of the saved vault; it includes saved sessions, stored SSH keys, stored AI audit history and Secure MCP Tunnel credentials. The original master password is required to open it. Existing destination files are never overwritten.
- **Import vault:** Close other RTerm windows and remote tabs, finish pending operations, and select Import vault. Choose a .rterm-vault or compatible version-2 vault.dat file and enter its original master password. Import replaces, rather than merges, the saved vault. If a vault already exists, unlock it first. RTerm authenticates the chosen backup, saves the previous encrypted vault as vault-before-import-*.rterm-vault in the configuration directory, then replaces the vault and locks the screen. Unlock using the imported vault's master password. The agent is stopped and tunnel auto-connect disabled; re-enable it after reviewing the restored profile.

The 128 MiB backup limit applies to both operations. Backup files are sensitive even though they are encrypted; store them securely and keep the master password separately. App appearance, non-vault settings, command history outside the vault, known hosts, and external SSH key files referenced by path are not included. Review paths and permissions when moving between operating systems. Older automatic backup files remain until you explicitly delete them.

## Installation

The repository includes native installer recipes and a **Build installers** GitHub Actions workflow. It builds both matching binaries from the same commit.

| Platform | Package | Default installation |
|---|---|---|
| Windows x64 | Setup EXE | Per-user Programs/RTerm; adds the installation folder to user PATH and provides a Start menu shortcut |
| macOS (Apple silicon or Intel) | PKG | /Applications/RTerm.app; CLI symlink in /usr/local/bin |
| Linux x64 | DEB | /usr/bin and desktop application menu |
| Linux x64 | tar.gz | Extract and run sh install.sh; defaults to ~/.local/bin (or /usr/local/bin as root; optional PREFIX) |

Linux packages are built on Ubuntu 24.04 and require compatible glibc and graphics libraries. They are not universal across every Linux distribution. Every RTerm installer bundles the same latest stable, checksum-verified official OpenAI tunnel-client for its target architecture, with upstream license notices. The client is installed beside RTerm on Windows and in the macOS app bundle, or in the private lib/rterm directory on Linux. No separate network download or manual client selection is needed at install time. Hosted CI produces ad-hoc signed macOS and unsigned Windows test packages. See [release builds](#release-builds) for trusted distribution.

The enabled AI agent continues to service requests when the RTerm window is
minimized or obscured, independently of rendering. Background SSH execution
does not raise the window. When a request needs interactive approval, RTerm
selects the appropriate SSH tab for foreground execution and asks the desktop
to restore and activate its window. Wayland compositors and other window
managers may refuse focus stealing or show an attention notification instead;
click that notification or restore RTerm to review the approval. Interactive
commands still require local approval; background access settings, vault
locking, cancellation and permission expiry remain in force.

Desktop shortcuts are optional. The Windows installer offers a **Create a desktop shortcut** checkbox (unchecked by default). The Linux portable installer asks when run interactively as a normal user. The macOS PKG and Linux DEB offer a one-time shortcut choice on first launch as the actual desktop user, rather than creating shortcuts in the installing administrator's home. The macOS shortcut links to the installed app; Linux creates an executable desktop launcher in the user's configured desktop directory, if available. Some Linux desktops require manually allowing launch of desktop files. Existing shortcuts are not overwritten by the Unix setup, and declining the offer leaves the desktop unchanged.

Application menu registration is automatic, separate from desktop shortcuts: Windows adds a Start menu entry, macOS installs a Utilities-category app bundle discoverable in Applications and system app search, and Linux registers an XDG desktop entry for GNOME, KDE Plasma and other compatible launchers. The portable Linux installer registers the entry in the user's XDG data directory (or the system local applications directory for elevated installs), with an absolute executable path even when a custom PREFIX is used. Search for **RTerm**, **Terminal**, or **SSH** in the Linux application launcher. Linux registers the bundled RTerm icon with the desktop theme. Dock, taskbar and favorites pinning remain user choices.

The shared command is `rterm-agent --version`. Restart your terminal or AI client after installing so it sees PATH changes. On macOS ensure /usr/local/bin is on PATH; on Linux portable installs ensure ~/.local/bin (or /usr/local/bin for system installs, or PREFIX/bin) is on PATH. When installing as root, choose a trusted system-owned prefix accessible to desktop users. Development builds can invoke the companion in target/debug or target/release directly; no checkout or username is embedded in the copied prompt.

Maintainers: **Build installers** runs on pushes to main and can also be started manually. Releases from main publish only after the audit and every native installer job succeed. Each release is named RTerm # followed by the short commit ID. Its tag uses the full commit ID. Enable release immutability in GitHub repository Settings > General > Releases before publishing. The workflow uploads all installers and verified checksums to a draft first and checks the immutable flag after publication. Existing releases are never overwritten.

### Software updates

Open **Settings → Update → Check for updates**. RTerm shows the installed version and build commit, checks the latest official immutable GitHub release, and compares same-version releases by commit ancestry. Older or divergent same-version development builds are not offered as automatic upgrades. Select **Install update…**, then **Download and install** to confirm. Downloads are verified against the SHA-256 digest published by GitHub before any installer is run.

On x86-64 Linux, the standard per-user portable installation in `~/.local` is updated without stopping RTerm; finish transfers and restart the application afterward so the desktop and agent use matching binaries. DEB installations receive a verified package for the system installer; other Linux prefixes and package managers use their normal update method. Windows and macOS launch the verified native installer, which may ask you to close RTerm or authorize installation. Save your work first. The updater uses the system HTTPS curl client and never requests administrator privileges itself. Transient DNS and connection failures are retried within a bounded time; if curl repeatedly fails to resolve an official GitHub hostname, RTerm also tries the native OS resolver and uses those addresses for that request only. HTTPS certificate checks and release digest verification remain mandatory. The Mac updater selects the package matching the running build architecture (Apple Silicon or Intel) and opens it in macOS Installer. No DNS settings are modified. The Release notes link is available if a manual update is needed. If no update is offered, hover over the status for version and branch details.

## Build and run

Install stable Rust and the platform’s C toolchain, then run these commands from the repository root.

```sh
cargo run --locked
cargo build --release --locked
cargo test --locked
```

Development builds retain application source line tables for backtraces
and omit dependency debug information to reduce clean build time and disk
usage. Release optimization (including thin LTO) is unchanged. For a
development build with full debug information, override the profile:

```sh
cargo build --locked --config 'profile.dev.debug=2' --config 'profile.dev.package."*".debug=2'
```

Unit tests and fixtures live in [tests/unit](../tests/README.md), rather
than inside production source files. Run the normal Cargo test commands
to execute them.

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

Packaging requires Bash, curl, jq, unzip, and SHA-256 utilities (sha256sum or shasum). Windows uses Git Bash from Git for Windows; jq must be available on PATH. Each commit-triggered installer workflow resolves the latest stable official OpenAI tunnel-client release once and shares its version and published SHA-256 digests across all platform jobs. Local packaging resolves latest by default too. CI authenticates release metadata requests with its built-in read-only GitHub token to avoid anonymous API rate limits; local builds can optionally set GH_TOKEN. Asset downloads do not receive the token. Missing assets or hashes, or a checksum mismatch, fail the build instead of silently using an older version. For an offline repeat build, first save the release snapshot with `bash packaging/fetch_tunnel_client.sh --resolve manifest.json`, then set `RTERM_TUNNEL_MANIFEST` to that snapshot and `RTERM_TUNNEL_ARCHIVE` to its matching upstream ZIP.

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

Verify installs and upgrades and uninstalls plus local terminal and SSH and SFTP and vault and language and agent permission behavior. Legacy version-2 vaults are upgraded to per-vault random derivation salts after a successful unlock; imported legacy backups upgrade on their next unlock. Publish only artifacts built from the intended committed revision with matching SHA-256 checksums.

## SSH host verification

Interactive SSH requests a UTF-8 PTY and the character locale LC_CTYPE=C.UTF-8. The server may decline locale requests or override them in its shell startup files. If non-ASCII input still fails, run `locale charmap` in the remote shell; it should report UTF-8. On Linux, check `locale -a` for an installed UTF-8 locale and configure your remote shell to use it. This does not translate terminal data or change server configuration.

On first connection, compare the displayed SHA-256 fingerprint with the server's fingerprint from a trusted source. **Trust and Connect** remembers the key for that host and port. RTerm checks the key before sending authentication credentials on each connection, including automatic reconnects. A changed key stops the connection and displays both fingerprints. Use **Trust Replacement and Connect** after verifying a planned rotation.

**SSH Keys → Known Hosts…** opens a resizable, searchable table of trusted hosts. Search by host, port, key type or fingerprint; multiple search words must all match. Click column headings to sort, or click a hostname or checkbox to select a row. Hover over a truncated value to see it in full, and select one host to copy its fingerprint. **Select matching** selects only the current search results. Changing the search or refreshing clears the selection. **Forget selected** asks for confirmation before removing any keys. The removal compares every selected key to the current trust store and writes the batch atomically; if any selected key has changed or disappeared, none are removed. Existing SSH connections remain open, but forgotten keys must be verified on the next connection. The list loads once when opened, refreshes after local trust changes, and renders only visible rows. Use **Refresh** to see changes made by another RTerm instance. The host-key verification policy and existing storage format are unchanged.

## Split terminal view

Use **View → Split Right (New Terminal)** or **Split Down (New Terminal)** to start a new local terminal beside or below the focused terminal. To display an existing SSH, local, Telnet or serial session instead, right-click its tab and choose **Split Right** or **Split Down**. Splits can be nested, with up to four visible main panes.

Click a pane to focus it; keyboard input goes to that pane only, and session-specific controls follow the active session. Scroll over a pane to browse its output independently. Drag a divider to resize its adjacent panes. Selecting an existing tab that is not visible replaces only the focused pane; the other panes remain visible. Drag a session tab onto a pane to place it there. Dropping a visible session on another visible session swaps their positions without reconnecting either one.

Use the pane header's remove button, the tab's **Remove from Split** action, or **View → Single Pane** to change the layout without disconnecting sessions. Closing a session tab still disconnects it. When the workspace is too small, only the focused pane is shown until enough space is available again. Split layouts last for the current application run. Main terminals share the configured font size; the bottom shell retains its own font setting.

## Free Type Mode and application mouse support

**View → Free Type Mode** (also in **Settings → Terminal**) enables mouse editing of a verified, echoed command in a bracketed-paste-capable single-line shell editor such as Bash/Readline. Click within the command to move its caret, drag to select text, type to replace the selection, or drag the selected text to a new position on the same command line. These actions send editing input to the actual shell; they do not modify screen pixels or execute the command. Press Enter explicitly to run it.

RTerm only edits known printable ASCII input that matches the current visible command. It does not guess editing operations for password prompts, scrollback, wrapped lines, completion/history text it cannot verify, or unknown application modes.

Applications that enable xterm mouse tracking receive clicks, button drags, releases, and wheel events using their negotiated SGR, UTF-8, or legacy encoding. For Vim/Neovim, enable `set mouse=a`; for terminal Emacs, enable `xterm-mouse-mode`. Each application determines its own editing and text-drag behavior. Hold Shift to select terminal output or open the terminal context menu instead of passing the mouse to an application. Free Type Mode can be disabled independently of application mouse tracking.

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

An upload is published using the server's rename operation. If the server refuses to replace an existing target, the completed partial remains available for retry after you resolve the destination conflict. Directory downloads require a new destination directory rather than merging with an existing directory. Directory transfers and agent-authorized SFTP uploads do not use single-file transfer recovery.

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
- `tests/unit/` contains the unit tests and fixtures, separated from production code.
- `vendor/eframe/` pins the existing eframe 0.33.3 API with a small native
  event-loop patch for non-rendering agent service and window activation.
  See `vendor/eframe/RTerm-PATCH.md` for the patch provenance and limitations.

The bundled fonts are distributed under their respective SIL Open Font Licenses in `assets/`.

### Bash prompt cursor after resizing

Bash 5.3 with older bundled Readline 8.3 patches can misplace its caret inside a colored prompt after a terminal narrows enough to wrap that prompt and then widens again. This occurs independently of RTerm. GNU Bash patch `bash53-020` (Readline patch 6) corrects prompt wrap calculations on resize. Use an updated shell when available; RTerm continues to honor cursor movement sequences rather than overriding application cursor positions. The opt-in test `bash_prompt_survives_repeated_resolution_changes` checks the affected shell behavior.

## SFTP-only sessions and the file manager

Choose **SFTP** in the Connect dialog for file access without an interactive shell. It uses the same SSH password/key/agent authentication, known-host verification, saved-profile vault and reconnect behavior. SFTP-only sessions are not exposed as shell-command targets to the AI agent. Protocol icons distinguish Local (green), SSH (blue), SFTP (gold), Telnet (purple) and Serial (orange).

The **Expand Filer** icon next to Refresh (or **View → File Manager**) opens a full-window, two-pane local/SFTP workspace. Drag the divider to resize the file panes. The terminal sessions and bottom shell remain connected when hidden; restoring the filer returns to the terminal workspace. SFTP-only sessions stay in a file workspace, and the restore control switches them to the compact tabbed file view.

Double-click a folder or type a directory in either address field, then press Enter. Drag files or folders from local to remote to upload, or remote to local to download. Drop onto a folder or an empty portion of the destination listing. Hold over a folder for approximately 650 ms to enter it while still dragging. Within one side, dropping into another directory opens a move confirmation. Ctrl/Cmd-click or Select Mode selects multiple items. Incoming desktop file drops on the remote list also upload, when supported by the window system. Outgoing native file-manager drag is not implemented.

The compact transfer progress card floats above the file lists without changing their height. Click the always-visible **Transfer queue** bar along the bottom of the file workspace (or its header icon) to show or hide pending, active, completed, skipped and failed transfers. Drag the horizontal divider above the open queue to resize it; its chosen height is retained while switching views. The collapsed bar still shows transfer counts, including failures. Click All, Pending, Completed, Failed or Skipped to open the queue with that filter; click the Transfer queue label to collapse it. Active desktop transfers display the measured payload rate in B/s, KiB/s or MiB/s, alongside progress and readable byte totals. Upload rates sample newly transmitted bytes; folder uploads show the current file, and resumed uploads exclude bytes already saved on the server. Completed history is limited to 200 entries and is not persisted across restarts.

Uploads and downloads check for an existing destination before starting. Choose **Replace**, **Keep both** (a numbered filename), or **Skip**. Enable **Always use this action for the current transfer batch** to reuse that decision for the remaining conflicts in the same batch. Dismissing the dialog skips the file. Replace is limited to regular files; existing folders can be kept separately or skipped, not merged or replaced. A late collision is still rejected rather than silently overwritten. Replacement data is staged first; remote replacement temporarily renames the original to a backup and attempts to restore it if publication fails. A connection failure can leave a backup on the server. Directory transfers can leave successfully copied entries in place if a later entry fails; retry individual missing entries. Ordinary file-picker uploads and downloads retain optional resume support for non-replacement transfers. Manual uploads accept readable regular files in shared or group-writable directories without requiring permission changes; symbolic links and special files are still rejected. AI-agent upload authorization, private-source checks and non-overwrite behavior are unchanged.

Click the **Date Modified** heading to change the sort direction, or its filter icon to open an inclusive date range. Drag either endpoint to adjust the filter. Folders remain visible for navigation, and filtered-out files are removed from the active selection. Filters are per listing and reset when navigating to a different directory.

Use the **New Folder** (folder-plus) button beside Refresh in the SFTP toolbar, or in the File Manager header to name a directory inside the current remote folder. This works in the expanded and docked file manager; the action is unavailable while connecting or loading a directory. Existing destinations are not replaced. The Local files toolbar also has a **New Local Folder** (folder-plus) button that creates a named directory inside the current local folder, even without an SSH connection. Click folder-plus to edit a new folder row directly in either file list. Type the name and press Enter to create it, or Escape to cancel. No temporary directory is created before confirmation. The listing refreshes after creation.

The download dialog previews selected items, allows an editable destination, and provides shortcuts for the current local folder and Downloads. Move and rename dialogs show their source items; remote move offers parent/source navigation and folders from the current listing.
