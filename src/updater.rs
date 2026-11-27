//! User-initiated updates from RTerm's immutable official GitHub releases.
//! No installer runs until an explicit second confirmation in Settings.
use std::{
    fs,
    net::{IpAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use eframe::egui::{self, RichText};
use serde::Deserialize;

use crate::{i18n::tr, theme::Theme};

const REPOSITORY: &str = "https://github.com/libalpm64/RTerm";
const API: &str = "https://api.github.com/repos/libalpm64/RTerm";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REVISION: Option<&str> = option_env!("RTERM_BUILD_COMMIT");
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const MAX_METADATA: u64 = 2 * 1024 * 1024;
const MAX_ATTEMPTS: usize = 4;
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const METADATA_TIMEOUT: Duration = Duration::from_secs(45);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Release {
    version: String,
    revision: String,
    name: String,
    digest: String,
    size: u64,
}

impl Release {
    fn url(&self) -> String {
        format!(
            "{REPOSITORY}/releases/download/rterm-{}/{}",
            self.revision, self.name
        )
    }
    fn description(&self) -> String {
        format!("{} · {}", self.version, &self.revision[..12])
    }
}

#[derive(Deserialize)]
struct PublishedRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    immutable: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct Comparison {
    status: String,
}

fn hex(s: &str, length: usize) -> bool {
    s.len() == length
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn version_tuple(s: &str) -> Option<[u64; 3]> {
    let parts: Vec<_> = s.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|p| {
            p.is_empty()
                || !p.bytes().all(|b| b.is_ascii_digit())
                || (p.len() > 1 && p.starts_with('0'))
        })
    {
        return None;
    }
    Some([
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ])
}

fn select_release(json: &[u8], suffix: &str) -> Result<Release, String> {
    let published: PublishedRelease =
        serde_json::from_slice(json).map_err(|e| format!("Invalid release metadata: {e}"))?;
    let revision = published
        .tag_name
        .strip_prefix("rterm-")
        .filter(|s| hex(s, 40))
        .ok_or("Unexpected RTerm release tag")?;
    if published.draft || published.prerelease || !published.immutable {
        return Err("The latest release is not a stable immutable RTerm release.".into());
    }
    let mut matches = published.assets.iter().filter_map(|asset| {
        let version = asset.name.strip_prefix("RTerm-")?.strip_suffix(suffix)?;
        version_tuple(version)?;
        Some((asset, version))
    });
    let (asset, version) = matches
        .next()
        .ok_or("No installer for this platform in the latest release.")?;
    if matches.next().is_some()
        || asset.state != "uploaded"
        || !(1..=MAX_PACKAGE).contains(&asset.size)
    {
        return Err("Invalid or ambiguous release installer.".into());
    }
    let digest = asset
        .digest
        .as_deref()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| hex(s, 64))
        .ok_or("The release installer has no valid SHA-256 digest.")?;
    let release = Release {
        version: version.into(),
        revision: revision.into(),
        name: asset.name.clone(),
        digest: digest.into(),
        size: asset.size,
    };
    if asset.browser_download_url != release.url() {
        return Err("Unexpected release download URL.".into());
    }
    Ok(release)
}

#[derive(Debug)]
enum Check {
    Current(String),
    Available(Release),
}

fn assess(
    release: Release,
    current_version: &str,
    current_revision: Option<&str>,
    compare: impl FnOnce(&str, &str) -> Result<String, String>,
) -> Result<Check, String> {
    let current = version_tuple(current_version).ok_or("Unknown installed version.")?;
    let latest = version_tuple(&release.version).ok_or("Invalid release version.")?;
    if latest < current {
        return Ok(Check::Current(
            "The installed version is newer than the latest public release.".into(),
        ));
    }
    if current_revision == Some(release.revision.as_str()) {
        return Ok(Check::Current("RTerm is up to date.".into()));
    }
    if latest > current {
        return Ok(Check::Available(release));
    }
    // Same-version releases are frequent. Only accept a descendant of the
    // installed commit; never switch a beta/divergent build to older main.
    let Some(current_revision) = current_revision.filter(|s| hex(s, 40)) else {
        return Ok(Check::Current(
            "Same version available; this build has no commit information to verify an upgrade."
                .into(),
        ));
    };
    match compare(current_revision, &release.revision)?.as_str() {
        "ahead" => Ok(Check::Available(release)),
        "identical" => Ok(Check::Current("RTerm is up to date.".into())),
        "behind" => Ok(Check::Current(
            "This build is newer than the latest public release.".into(),
        )),
        "diverged" => Ok(Check::Current(
            "This build is on a different development branch. No automatic update is offered."
                .into(),
        )),
        _ => Err("Unexpected release comparison result.".into()),
    }
}

#[derive(Clone, Debug)]
enum Target {
    #[cfg(target_os = "linux")]
    Portable(PathBuf),
    #[cfg(target_os = "linux")]
    Debian,
    #[cfg(target_os = "macos")]
    Mac,
    #[cfg(target_os = "windows")]
    Windows,
}

impl Target {
    fn detect() -> Result<Self, String> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let home = crate::storage::home_dir();
            let prefix = home.join(".local");
            if unsafe { libc::geteuid() } != 0
                && home.is_absolute()
                && exe == prefix.join("bin/rterm")
                && prefix.join("bin/rterm-agent").is_file()
            {
                return Ok(Self::Portable(prefix));
            }
            if exe == Path::new("/usr/bin/rterm")
                && Command::new("/usr/bin/dpkg-query")
                    .args(["-S", "/usr/bin/rterm"])
                    .output()
                    .is_ok_and(|out| {
                        out.status.success()
                            && String::from_utf8_lossy(&out.stdout).lines().any(|line| {
                                line == "rterm: /usr/bin/rterm"
                                    || line == "rterm:amd64: /usr/bin/rterm"
                            })
                    })
            {
                return Ok(Self::Debian);
            }
            Err("This Linux installation is not the standard per-user portable or DEB installation. Use the official release page or your package manager.".into())
        }
        #[cfg(all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ))]
        {
            Ok(Self::Mac)
        }
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            Ok(Self::Windows)
        }
        #[cfg(not(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(
                target_os = "macos",
                any(target_arch = "aarch64", target_arch = "x86_64")
            ),
            all(target_os = "windows", target_arch = "x86_64")
        )))]
        {
            Err("No update package is available for this platform.".into())
        }
    }

    fn suffix(&self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::Portable(_) => "-linux-amd64.tar.gz",
            #[cfg(target_os = "linux")]
            Self::Debian => "-linux-amd64.deb",
            #[cfg(target_os = "macos")]
            Self::Mac => macos_package_suffix(cfg!(target_arch = "aarch64")),
            #[cfg(target_os = "windows")]
            Self::Windows => "-windows-x64-setup.exe",
        }
    }
}

#[cfg(any(test, target_os = "macos"))]
fn macos_package_suffix(apple_silicon: bool) -> &'static str {
    if apple_silicon {
        "-macos-arm64.pkg"
    } else {
        "-macos-x64.pkg"
    }
}

fn curl() -> Result<Command, String> {
    #[cfg(windows)]
    let path = {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .ok_or("Windows system directory is unavailable.")?;
        if !root.is_absolute() {
            return Err("Invalid Windows system directory.".into());
        }
        root.join("System32/curl.exe")
    };
    #[cfg(not(windows))]
    let path = PathBuf::from("/usr/bin/curl");
    if !path.is_file() {
        return Err("The system curl HTTPS client is required to check for updates.".into());
    }
    let mut cmd = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    // Ignore user curl config (including flags that could disable TLS checks).
    cmd.args([
        "--disable",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--location",
        "--max-redirs",
        "5",
        "--fail",
        "--silent",
        "--show-error",
        "--connect-timeout",
        "10",
        "--user-agent",
        "RTerm-Updater",
    ])
    .stdin(Stdio::null());
    Ok(cmd)
}

// Only the official GitHub release hosts may be given per-request DNS
// overrides. The URL, Host header, SNI and TLS certificate checks are never
// changed. Addresses come from the operating system, not hardcoded IPs or
// an external DNS service.
fn failed_dns_host(stderr: &str) -> Option<&'static str> {
    const HOSTS: &[&str] = &[
        "api.github.com",
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
        "github-releases.githubusercontent.com",
    ];
    for line in stderr.lines() {
        if let Some(host) = line
            .split_once("Could not resolve host: ")
            .map(|(_, host)| host.trim())
        {
            return HOSTS
                .iter()
                .copied()
                .find(|allowed| host.eq_ignore_ascii_case(allowed));
        }
    }
    None
}

fn resolve_argument(host: &str, addresses: &[IpAddr]) -> Option<String> {
    let mut unique = Vec::new();
    for address in addresses {
        if !unique.contains(address) {
            unique.push(*address);
        }
    }
    if unique.is_empty() {
        return None;
    }
    let values = unique
        .iter()
        .map(|address| match address {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        })
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("{host}:443:{values}"))
}

fn system_resolve(host: &str) -> Result<Vec<IpAddr>, String> {
    // getaddrinfo uses the native resolver on Linux, macOS and Windows.
    // Do not keep an updater waiting on a stalled resolver indefinitely.
    // Bound the number of outstanding native lookups even on repeated clicks.
    static RESOLVING: AtomicBool = AtomicBool::new(false);
    if RESOLVING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("The operating system is still resolving a previous request.".into());
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RESOLVING.store(false, Ordering::Release);
        }
    }
    let reset = Reset;
    let host = host.to_owned();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("rterm-update-dns".into())
        .spawn(move || {
            let _reset = reset;
            let result = (host.as_str(), 443)
                .to_socket_addrs()
                .map(|addresses| addresses.map(|address| address.ip()).collect::<Vec<_>>())
                .map_err(|error| error.to_string());
            let _ = tx.send(result);
        })
        .map_err(|error| error.to_string())?;
    rx.recv_timeout(DNS_TIMEOUT)
        .map_err(|_| "Operating system DNS lookup timed out.".to_owned())?
}

#[derive(Debug)]
struct Attempt {
    code: Option<i32>,
    body: Vec<u8>,
    error: String,
}

impl From<Output> for Attempt {
    fn from(output: Output) -> Self {
        Self {
            code: output.status.code(),
            body: output.stdout,
            error: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

fn retryable(code: Option<i32>) -> bool {
    // Proxy/host DNS, connect, partial transfer, timeout, empty reply,
    // transient send/receive and HTTP/2 stream failures. Do not retry TLS
    // validation, HTTP status errors, or filesystem/size errors.
    matches!(code, Some(5 | 6 | 7 | 18 | 28 | 52 | 55 | 56 | 92))
}

fn connection_error(result: &Attempt) -> String {
    match result.code {
        Some(6) => {
            let host = failed_dns_host(&result.error).unwrap_or("GitHub");
            format!("DNS lookup for {host} failed after retrying.")
        }
        Some(5) => "Could not resolve the network proxy.".into(),
        Some(7 | 18 | 28 | 52 | 55 | 56 | 92) => "Could not connect to GitHub.".into(),
        Some(51 | 60 | 77) => "GitHub HTTPS certificate verification failed.".into(),
        Some(code) => format!(
            "GitHub HTTPS request failed (curl error {code}). {}",
            result.error.trim().chars().take(400).collect::<String>()
        ),
        None => "The system HTTPS client terminated unexpectedly.".into(),
    }
}

fn retry_request(
    mut attempt: impl FnMut(&[String]) -> Result<Attempt, String>,
    mut resolve: impl FnMut(&str) -> Result<Vec<IpAddr>, String>,
    mut pause: impl FnMut(Duration),
) -> Result<Vec<u8>, String> {
    let mut mappings: Vec<String> = Vec::new();
    let mut resolved = Vec::new();
    for index in 0..MAX_ATTEMPTS {
        let result = attempt(&mappings)?;
        if result.code == Some(0) {
            return Ok(result.body);
        }
        if !retryable(result.code) || index + 1 == MAX_ATTEMPTS {
            return Err(connection_error(&result));
        }
        // Give an ordinary transient failure one fresh request before
        // consulting getaddrinfo separately. A download redirect can fail
        // DNS on a second hostname, so retain each verified-host mapping
        // for this request only.
        if result.code == Some(6)
            && index > 0
            && let Some(host) = failed_dns_host(&result.error)
            && !resolved.contains(&host)
        {
            resolved.push(host);
            if let Ok(addresses) = resolve(host)
                && let Some(argument) = resolve_argument(host, &addresses)
            {
                mappings.push(argument);
            }
        }
        pause(Duration::from_millis(if index == 0 { 350 } else { 750 }));
    }
    unreachable!("retry count is nonzero")
}

fn https_request(
    url: &str,
    arguments: &[&std::ffi::OsStr],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + timeout;
    retry_request(
        |mappings| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("GitHub HTTPS request timed out. Try again.".into());
            }
            let mut command = curl()?;
            command.args([
                "--max-time",
                &format!("{:.3}", remaining.as_secs_f64().max(0.001)),
            ]);
            for mapping in mappings {
                command.args(["--resolve", mapping]);
            }
            command.args(arguments).arg(url);
            command
                .output()
                .map(Attempt::from)
                .map_err(|error| format!("Could not start the system HTTPS client: {error}"))
        },
        system_resolve,
        std::thread::sleep,
    )
}

fn metadata(url: &str) -> Result<Vec<u8>, String> {
    let max = MAX_METADATA.to_string();
    let arguments = [
        "--max-filesize",
        &max,
        "--header",
        "Accept: application/vnd.github+json",
        "--header",
        "X-GitHub-Api-Version: 2022-11-28",
    ]
    .map(std::ffi::OsStr::new);
    let result = https_request(url, &arguments, METADATA_TIMEOUT)
        .map_err(|error| format!("Update check failed: {error}"))?;
    if result.len() as u64 > MAX_METADATA {
        return Err("Update check failed: GitHub returned too much release metadata.".into());
    }
    Ok(result)
}

fn check(target: &Target) -> Result<Check, String> {
    let release = select_release(
        &metadata(&format!("{API}/releases/latest"))?,
        target.suffix(),
    )?;
    let latest_description = release.description();
    let result = assess(release, VERSION, REVISION, |current, latest| {
        let response = metadata(&format!("{API}/compare/{current}...{latest}?per_page=1"))?;
        let comparison: Comparison = serde_json::from_slice(&response)
            .map_err(|e| format!("Invalid commit comparison: {e}"))?;
        Ok(comparison.status)
    })?;
    Ok(match result {
        Check::Current(message) => Check::Current(format!(
            "{message}\nLatest public release: {latest_description}"
        )),
        available => available,
    })
}

fn sha256(bytes: &[u8]) -> String {
    ssh_key::HashAlg::Sha256
        .digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn verify_package(path: &Path, release: &Release) -> Result<(), String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size != release.size || size > MAX_PACKAGE {
        return Err("Downloaded installer size does not match the release.".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != size || sha256(&bytes) != release.digest {
        return Err(
            "Downloaded installer failed SHA-256 verification. Nothing was installed.".into(),
        );
    }
    Ok(())
}

fn run(mut command: Command, description: &str) -> Result<(), String> {
    let result = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{description}: {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "{description}: {} {}",
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(())
}

fn download_and_install(target: Target, release: Release) -> Result<String, String> {
    let work = tempfile::Builder::new()
        .prefix("rterm-update-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let package = work.path().join(&release.name);
    // A new curl process opens --output from the beginning on every
    // attempt. Never append to partial downloads or install unverified data.
    let size = release.size.to_string();
    https_request(
        &release.url(),
        &[
            std::ffi::OsStr::new("--max-filesize"),
            std::ffi::OsStr::new(&size),
            std::ffi::OsStr::new("--output"),
            package.as_os_str(),
        ],
        DOWNLOAD_TIMEOUT,
    )
    .map_err(|error| format!("Download failed: {error}"))?;
    verify_package(&package, &release)?;

    match target {
        #[cfg(target_os = "linux")]
        Target::Portable(prefix) => {
            let _lock = lock_portable(&prefix)?;
            // Recheck the installation identity after download. Never change
            // a system package, development tree or another user's prefix.
            if !matches!(Target::detect()?, Target::Portable(current) if current == prefix) {
                return Err("The installation location changed. Check for updates again.".into());
            }
            if let Err(error) = install_portable(&package, work.path(), &prefix) {
                let directory = work.keep();
                return Err(format!(
                    "{error}\nUpdate files retained for recovery at {}",
                    directory.display()
                ));
            }
            Ok("Update installed. Restart RTerm when ready.".into())
        }
        #[cfg(target_os = "linux")]
        Target::Debian => {
            // The system package manager owns this installation. Hand off the
            // verified DEB, never elevate privileges or replace it ourselves.
            let directory = work.keep();
            let mut command = Command::new("/usr/bin/xdg-open");
            command.arg(&directory);
            run(command, "Could not open the downloaded package directory")?;
            Ok(format!(
                "Verified package downloaded to {}. Open it with your system package installer to finish updating.",
                directory.join(release.name).display()
            ))
        }
        #[cfg(target_os = "macos")]
        Target::Mac => {
            let directory = work.keep();
            let mut command = Command::new("/usr/bin/open");
            command
                .args(["-a", "Installer"])
                .arg(directory.join(release.name));
            run(command, "Could not open macOS Installer")?;
            Ok("Installer opened. Follow the installation instructions.".into())
        }
        #[cfg(target_os = "windows")]
        Target::Windows => {
            let status = Command::new(&package)
                .current_dir(work.path())
                .stdin(Stdio::null())
                .status()
                .map_err(|e| format!("Could not start the Windows installer: {e}"))?;
            if !status.success() {
                return Err(format!("Windows installer exited with {status}."));
            }
            Ok("Update installed. Restart RTerm when ready.".into())
        }
    }
}

#[cfg(target_os = "linux")]
fn lock_portable(prefix: &Path) -> Result<fs::File, String> {
    use std::os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    };
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(prefix.join("bin/.rterm-update.lock"))
        .map_err(|e| format!("Could not open updater lock: {e}"))?;
    let info = lock.metadata().map_err(|e| e.to_string())?;
    if !info.is_file() || info.uid() != unsafe { libc::geteuid() } {
        return Err("Invalid updater lock file.".into());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Another RTerm update is already installing.".into());
    }
    Ok(lock)
}

#[cfg(target_os = "linux")]
fn install_portable(package: &Path, work: &Path, prefix: &Path) -> Result<(), String> {
    let archive = work.join("archive");
    fs::create_dir(&archive).map_err(|e| e.to_string())?;
    let mut tar = Command::new("/usr/bin/tar");
    tar.env("PATH", "/usr/bin:/bin")
        .env_remove("TAR_OPTIONS")
        .env_remove("GZIP");
    tar.args([
        "--extract",
        "--gzip",
        "--no-same-owner",
        "--no-same-permissions",
        "--file",
    ])
    .arg(package)
    .arg("--directory")
    .arg(&archive);
    run(tar, "Could not extract verified update")?;
    // Use the release's existing installer for the app, agent, tunnel client,
    // licenses and launcher together. It uses GNU install (new inodes), so
    // existing processes remain running. Back up the executable pair in case
    // installation or post-install verification fails.
    let bin = prefix.join("bin");
    let backup = work.join("backup");
    fs::create_dir(&backup).map_err(|e| e.to_string())?;
    for name in ["rterm", "rterm-agent"] {
        fs::copy(bin.join(name), backup.join(name))
            .map_err(|e| format!("Could not back up {name}: {e}"))?;
    }
    let mut installer = Command::new("/bin/sh");
    installer
        .arg(archive.join("install.sh"))
        .current_dir(&archive)
        .env("PREFIX", prefix)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("ENV")
        .env_remove("BASH_ENV");
    let result = run(installer, "Portable installer failed").and_then(|()| {
        for name in ["rterm", "rterm-agent"] {
            let expected = fs::read(archive.join(name)).map_err(|e| e.to_string())?;
            let actual = fs::read(bin.join(name)).map_err(|e| e.to_string())?;
            if expected != actual {
                return Err(format!("Installed {name} does not match the update."));
            }
        }
        Ok(())
    });
    if let Err(error) = result {
        let mut rollback_errors = Vec::new();
        for name in ["rterm", "rterm-agent"] {
            let restore = (|| -> std::io::Result<()> {
                let mut stage = tempfile::NamedTempFile::new_in(&bin)?;
                std::io::copy(&mut fs::File::open(backup.join(name))?, stage.as_file_mut())?;
                stage
                    .as_file()
                    .set_permissions(fs::metadata(backup.join(name))?.permissions())?;
                stage.persist(bin.join(name)).map_err(|e| e.error)?;
                Ok(())
            })();
            if let Err(restore_error) = restore {
                rollback_errors.push(format!("{name}: {restore_error}"));
            }
        }
        if !rollback_errors.is_empty() {
            return Err(format!(
                "{error}; executable rollback failed: {}. Reinstall from the official release page.",
                rollback_errors.join("; ")
            ));
        }
        return Err(format!(
            "{error}; previous RTerm and agent restored. Other installer files may need repair."
        ));
    }
    Ok(())
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Idle,
    Checking,
    Current(String),
    Available(Target, Release),
    Confirm(Target, Release),
    Installing {
        keep_open: bool,
    },
    Finished(String),
    Error(String),
}

#[derive(Default)]
pub(crate) struct Updater {
    state: State,
    pending: Option<mpsc::Receiver<State>>,
}

impl Updater {
    fn start(
        &mut self,
        context: &egui::Context,
        state: State,
        work: impl FnOnce() -> State + Send + 'static,
    ) {
        let (tx, rx) = mpsc::channel();
        let context = context.clone();
        match std::thread::Builder::new()
            .name("rterm-software-update".into())
            .spawn(move || {
                let _ = tx.send(work());
                context.request_repaint();
            }) {
            Ok(_) => {
                self.state = state;
                self.pending = Some(rx);
            }
            Err(e) => self.state = State::Error(format!("Could not start update worker: {e}")),
        }
    }

    pub(crate) fn poll(&mut self) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(state) => {
                    self.state = state;
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.state = State::Error("Update worker stopped unexpectedly.".into());
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    pub(crate) fn blocks_exit(&mut self) -> bool {
        self.poll();
        matches!(self.state, State::Installing { keep_open: true })
    }

    fn begin_check(&mut self, context: &egui::Context) {
        self.start(context, State::Checking, || {
            let result =
                Target::detect().and_then(|target| check(&target).map(|check| (target, check)));
            match result {
                Ok((target, Check::Available(release))) => State::Available(target, release),
                Ok((_, Check::Current(message))) => State::Current(message),
                Err(error) => State::Error(error),
            }
        });
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui, t: Theme) {
        self.poll();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            ui.label(RichText::new(tr(ui, "Installed version")).color(t.muted));
            let revision = REVISION.map(|r| &r[..12]).unwrap_or("unknown");
            ui.label(
                RichText::new(format!("{VERSION} · {revision}"))
                    .color(t.text)
                    .strong(),
            );
        });
        ui.add_space(12.0);
        let mut next = None;
        match &self.state {
            State::Idle => {}
            State::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr(ui, "Checking for updates…"));
                });
            }
            State::Current(message) => {
                // Keep the normal status short; preserve branch/commit
                // diagnostics in the tooltip rather than an always-visible note.
                ui.label(RichText::new(tr(ui, "No updates available")).color(t.green))
                    .on_hover_text(message);
            }
            State::Available(target, release) => {
                ui.label(
                    RichText::new(tr(ui, "Update available"))
                        .color(t.green)
                        .strong(),
                );
                ui.label(release.description());
                if ui.button(tr(ui, "Install update…")).clicked() {
                    next = Some(State::Confirm(target.clone(), release.clone()));
                }
            }
            State::Confirm(target, release) => {
                ui.label(RichText::new(tr(ui, "Install this update?")).strong());
                ui.label(release.description());
                #[cfg(target_os = "linux")]
                let portable = matches!(target, Target::Portable(_));
                #[cfg(not(target_os = "linux"))]
                let portable = false;
                ui.label(
                    RichText::new(tr(
                        ui,
                        if portable {
                            "Keep RTerm open until the update finishes."
                        } else {
                            "Save your work before installing."
                        },
                    ))
                    .color(t.muted),
                );
                ui.horizontal_wrapped(|ui| {
                    if ui.button(tr(ui, "Cancel")).clicked() {
                        next = Some(State::Available(target.clone(), release.clone()));
                    }
                    if ui.button(tr(ui, "Download and install")).clicked() {
                        #[cfg(target_os = "linux")]
                        let keep_open = matches!(target, Target::Portable(_));
                        #[cfg(not(target_os = "linux"))]
                        let keep_open = false;
                        next = Some(State::Installing { keep_open });
                    }
                });
            }
            State::Installing { keep_open } => {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    ui.label(tr(
                        ui,
                        if *keep_open {
                            "Installing… Keep RTerm open."
                        } else {
                            "Installing update…"
                        },
                    ));
                });
            }
            State::Finished(message) => {
                ui.label(RichText::new(tr(ui, message)).color(t.green));
            }
            State::Error(error) => {
                ui.label(RichText::new(error).color(t.red));
            }
        }
        if let Some(next) = next {
            if matches!(next, State::Installing { .. }) {
                if let State::Confirm(target, release) = std::mem::take(&mut self.state) {
                    self.start(ui.ctx(), next, move || {
                        match download_and_install(target, release) {
                            Ok(message) => State::Finished(message),
                            Err(error) => State::Error(error),
                        }
                    });
                }
            } else {
                self.state = next;
            }
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if matches!(
                self.state,
                State::Idle | State::Current(_) | State::Error(_)
            ) && ui.button(tr(ui, "Check for updates")).clicked()
            {
                self.begin_check(ui.ctx());
            }
            if !matches!(
                self.state,
                State::Checking | State::Installing { .. } | State::Confirm(..)
            ) && ui.link(tr(ui, "Release notes")).clicked()
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                    "{REPOSITORY}/releases/latest"
                )));
            }
        });
    }
}

#[cfg(test)]
#[path = "../tests/unit/updater.rs"]
mod tests;
