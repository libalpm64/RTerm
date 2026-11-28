use crate::models::{AgentAuditEntry, Connection, Profile, SshConfig, SshKey};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};

const VAULT_MAGIC: &[u8; 4] = b"RTVL";
const LEGACY_VAULT_VERSION: u16 = 2;
const VAULT_VERSION: u16 = 3;
const VAULT_SALT_LEN: usize = 16;

pub(crate) struct VaultKey([u8; 32], [u8; VAULT_SALT_LEN]);

impl Drop for VaultKey {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

impl VaultKey {
    pub(crate) fn derive(password: &str) -> Self {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let salt = nonce[..VAULT_SALT_LEN].try_into().unwrap();
        Self::derive_with_salt(password, salt)
    }

    fn derive_with_salt(password: &str, salt: [u8; VAULT_SALT_LEN]) -> Self {
        let mut key = [0u8; 32];
        argon2::Argon2::default()
            .hash_password_into(password.as_bytes(), &salt, &mut key)
            .expect("argon2 key derivation failed");
        Self(key, salt)
    }

    fn from_data(password: &str, data: &[u8]) -> Result<(Self, bool), String> {
        let version = vault_version(data)?;
        let salt = if version == LEGACY_VAULT_VERSION {
            legacy_vault_salt()
        } else {
            data[6..6 + VAULT_SALT_LEN].try_into().unwrap()
        };
        Ok((
            Self::derive_with_salt(password, salt),
            version == LEGACY_VAULT_VERSION,
        ))
    }
}

#[derive(Default)]
pub(crate) struct Vault {
    pub(crate) profiles: Vec<Profile>,
    pub(crate) session_folders: BTreeSet<String>,
    pub(crate) keys: Vec<SshKey>,
    pub(crate) agent_audit: VecDeque<AgentAuditEntry>,
    pub(crate) openai_tunnel: Option<crate::tunnel::Profile>,
}

/// Resolve user home on all supported platforms
pub(crate) fn home_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return std::path::PathBuf::from(profile);
        }
        if let (Some(drive), Some(path)) =
            (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
        {
            let mut home = std::path::PathBuf::from(drive);
            home.push(path);
            return home;
        }
    }

    // HOME can still be supplied by shells such as Git Bash on Windows
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

pub(crate) fn expand_local_path(path: &str) -> std::path::PathBuf {
    let path = path.trim();
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    std::path::PathBuf::from(path)
}

pub(crate) fn default_download_dir() -> std::path::PathBuf {
    home_dir().join("Downloads")
}

fn legacy_config_dirs() -> Vec<std::path::PathBuf> {
    // Migrate config from legacy working directory locations
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(std::path::Path::to_path_buf);
        for _ in 0..5 {
            let Some(current) = dir else { break };
            candidates.push(current.join("rterm"));
            dir = current.parent().map(std::path::Path::to_path_buf);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("rterm"));
    }
    // Include Git Bash and Unix style Windows paths
    candidates.push(home_dir().join(".config").join("rterm"));
    candidates
}

fn read_legacy_config_file(path: &Path) -> Option<Vec<u8>> {
    let parent = path.parent()?;
    #[cfg(unix)]
    if !unix_directory_is_private(parent) {
        return None;
    }
    #[cfg(windows)]
    if !windows_directory_is_not_shared_writable(parent) {
        return None;
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_file()
            || (metadata.uid() != uid && metadata.uid() != 0)
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
            || metadata.len() > MAX_CONFIG_FILE
        {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_FILE.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= MAX_CONFIG_FILE).then_some(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        options.custom_flags(0x00200000);
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0
            || !metadata.is_file()
            || metadata.file_attributes() & 0x400 != 0
            || info.nNumberOfLinks != 1
            || !windows_file_is_not_shared_writable(&file)
            || metadata.file_size() > MAX_CONFIG_FILE
        {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_FILE.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= MAX_CONFIG_FILE).then_some(bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_CONFIG_FILE {
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_CONFIG_FILE.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= MAX_CONFIG_FILE).then_some(bytes)
    }
}

fn migrate_legacy_config(config_dir: &std::path::Path) {
    if !ensure_private_config_dir(config_dir) {
        return;
    }
    let marker = config_dir.join(".legacy-migrated");
    if marker.exists() {
        return;
    }
    let names = ["vault.dat", "settings.json"];
    let candidates = legacy_config_dirs();
    for name in names {
        let destination = config_dir.join(name);
        if destination.exists() {
            continue;
        }
        for candidate in &candidates {
            let source = candidate.join(name);
            if source == destination {
                continue;
            }
            if let Some(bytes) = read_legacy_config_file(&source)
                && write_atomic(&destination, &bytes).is_ok()
            {
                break;
            }
        }
    }
    // Mark migration complete to prevent rediscovery
    let _ = write_atomic(&marker, b"1");
}

pub(crate) fn ensure_private_config_dir(config_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
        if let Some(parent) = config_dir.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return false;
        }
        if let Some(parent) = config_dir.parent()
            && !unix_directory_is_private(parent)
        {
            // Every ancestor must prevent another user from replacing the
            // private directory entry after it has been checked.
            return false;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(config_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return false,
        }
        // Open the directory itself without following a final symlink. Apply
        // permissions through the handle so a path swap cannot chmod a target.
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let Ok(directory) = options.open(config_dir) else {
            return false;
        };
        let Ok(metadata) = directory.metadata() else {
            return false;
        };
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return false;
        }
        metadata.permissions().mode() & 0o777 == 0o700
            || directory
                .set_permissions(std::fs::Permissions::from_mode(0o700))
                .is_ok()
    }
    #[cfg(windows)]
    {
        std::fs::create_dir_all(config_dir).is_ok() && windows_directory_is_private(config_dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::create_dir_all(config_dir).is_ok()
    }
}

#[cfg(windows)]
fn windows_check_directory(path: &Path, check: fn(&std::fs::File) -> bool) -> bool {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(0x02000000 | 0x00200000);
    options.open(path).is_ok_and(|directory| {
        directory.metadata().is_ok_and(|metadata| {
            metadata.is_dir() && metadata.file_attributes() & 0x400 == 0 && check(&directory)
        })
    })
}

#[cfg(windows)]
pub(crate) fn windows_directory_is_private(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut directories = path.ancestors().collect::<Vec<_>>();
    directories.reverse();
    directories.iter().enumerate().all(|(index, directory)| {
        let check = if index + 1 == directories.len() {
            windows_file_is_private
        } else {
            windows_directory_is_not_replaceable
        };
        windows_check_directory(directory, check)
    })
}

#[cfg(windows)]
pub(crate) fn windows_file_is_private(file: &std::fs::File) -> bool {
    windows_acl_is_safe(file, false, false)
}

#[cfg(windows)]
pub(crate) fn windows_file_is_not_shared_writable(file: &std::fs::File) -> bool {
    windows_acl_is_safe(file, true, false)
}

#[cfg(windows)]
fn windows_directory_is_not_replaceable(file: &std::fs::File) -> bool {
    windows_acl_is_safe(file, true, true)
}

#[cfg(windows)]
pub(crate) fn windows_directory_is_not_shared_writable(path: &Path) -> bool {
    path.is_absolute()
        && path.ancestors().all(|directory| {
            windows_check_directory(directory, windows_directory_is_not_replaceable)
        })
}

#[cfg(windows)]
fn windows_acl_is_safe(file: &std::fs::File, allow_read_only: bool, directory: bool) -> bool {
    use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Authorization::{ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
            GetSecurityDescriptorDacl, OWNER_SECURITY_INFORMATION,
        },
    };

    struct Allocation(*mut c_void);
    impl Drop for Allocation {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }

    fn sid(value: &str) -> Option<Allocation> {
        let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        let mut sid = ptr::null_mut();
        if unsafe { ConvertStringSidToSidW(value.as_ptr(), &mut sid) } == 0 || sid.is_null() {
            None
        } else {
            Some(Allocation(sid.cast()))
        }
    }

    unsafe {
        let mut owner = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        if GetSecurityInfo(
            file.as_raw_handle() as _,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        ) != 0
            || descriptor.is_null()
        {
            return false;
        }
        let descriptor = Allocation(descriptor.cast());
        let Ok(user) = crate::agent_ipc::user_sid() else {
            return false;
        };
        let Some(user_sid) = sid(&user) else {
            return false;
        };
        let Some(system_sid) = sid("S-1-5-18") else {
            return false;
        };
        let Some(admins_sid) = sid("S-1-5-32-544") else {
            return false;
        };
        let Some(installer_sid) =
            sid("S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464")
        else {
            return false;
        };
        if owner.is_null()
            || ![&user_sid, &system_sid, &admins_sid, &installer_sid]
                .iter()
                .any(|trusted| EqualSid(owner, trusted.0.cast()) != 0)
        {
            return false;
        }
        let Some(creator_sid) = sid("S-1-3-0") else {
            return false;
        };
        let trusted = [user_sid, system_sid, admins_sid, installer_sid, creator_sid];
        let mut present = 0;
        let mut defaulted = 0;
        if GetSecurityDescriptorDacl(descriptor.0.cast(), &mut present, &mut dacl, &mut defaulted)
            == 0
            || present == 0
            || dacl.is_null()
        {
            return false;
        }
        for index in 0..(*dacl).AceCount as u32 {
            let mut ace = ptr::null_mut();
            if GetAce(dacl, index, &mut ace) == 0 || ace.is_null() {
                return false;
            }
            let header = &*ace.cast::<ACE_HEADER>();
            // Inherit-only ACEs do not grant access to this directory/file;
            // their effective permissions are checked on each child instead.
            if header.AceFlags & 0x08 != 0 {
                continue;
            }
            match header.AceType {
                0 => {
                    let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    let trustee = ptr::addr_of!(allowed.SidStart).cast_mut().cast();
                    let trusted_principal = trusted
                        .iter()
                        .any(|allowed| EqualSid(trustee, allowed.0.cast()) != 0);
                    const WRITE_MASK: u32 = 0x40000000
                        | 0x10000000
                        | 0x00010000
                        | 0x00040000
                        | 0x00080000
                        | 0x00000002
                        | 0x00000004
                        | 0x00000010
                        | 0x00000100
                        | 0x00000040;
                    const DIRECTORY_REPLACE_MASK: u32 =
                        0x10000000 | 0x00010000 | 0x00040000 | 0x00080000 | 0x00000040;
                    let restricted = if directory {
                        DIRECTORY_REPLACE_MASK
                    } else {
                        WRITE_MASK
                    };
                    if !trusted_principal && (!allow_read_only || allowed.Mask & restricted != 0) {
                        return false;
                    }
                }
                1 => {}
                // Unknown ACE types may grant access through object or callback
                // semantics; reject them rather than silently weakening checks.
                _ => return false,
            }
        }
        true
    }
}

#[cfg(unix)]
fn safe_owned_directory(is_dir: bool, owner: u32, mode: u32, current_uid: u32) -> bool {
    is_dir && (owner == current_uid || owner == 0) && (mode & 0o022 == 0 || mode & 0o1000 != 0)
}

#[cfg(unix)]
pub(crate) fn unix_directory_is_private(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    // Check both the original path and every symlink target's ancestors.
    // Canonicalizing alone would hide an untrusted intermediate directory;
    // rejecting all symlinks would break system aliases such as macOS /var.
    fn resolve(path: &Path, uid: u32, links_left: &mut usize) -> Option<PathBuf> {
        if !path.is_absolute() {
            return None;
        }
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component);
            let metadata = std::fs::symlink_metadata(&current).ok()?;
            if metadata.file_type().is_symlink() {
                if (metadata.uid() != uid && metadata.uid() != 0) || *links_left == 0 {
                    return None;
                }
                *links_left -= 1;
                let target = std::fs::read_link(&current).ok()?;
                current.pop();
                current = resolve(&current.join(target), uid, links_left)?;
            } else if !safe_owned_directory(
                metadata.is_dir(),
                metadata.uid(),
                metadata.permissions().mode(),
                uid,
            ) {
                return None;
            }
        }
        Some(current)
    }

    resolve(path, unsafe { libc::geteuid() }, &mut 40).is_some()
}

pub(crate) fn get_config_dir() -> std::path::PathBuf {
    let config_dir = if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData").join("Roaming"))
            .join("rterm")
    } else if cfg!(target_os = "macos") {
        home_dir()
            .join("Library")
            .join("Application Support")
            .join("rterm")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("rterm")
    };
    migrate_legacy_config(&config_dir);
    config_dir
}

pub(crate) fn vault_exists() -> bool {
    get_config_dir().join("vault.dat").exists()
}

// Backups use the existing encrypted vault format, not a plaintext export.
const MAX_VAULT_BACKUP: u64 = 128 * 1024 * 1024;
const MAX_CONFIG_FILE: u64 = 16 * 1024 * 1024;

fn legacy_vault_salt() -> [u8; VAULT_SALT_LEN] {
    blake3::hash(b"rterm-vault-v2").as_bytes()[..VAULT_SALT_LEN]
        .try_into()
        .unwrap()
}

fn vault_version(data: &[u8]) -> Result<u16, String> {
    if data.len() < 6 || &data[..4] != VAULT_MAGIC {
        return Err("Not an RTerm encrypted vault".into());
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    let minimum = match version {
        LEGACY_VAULT_VERSION => 46,
        VAULT_VERSION => 6 + VAULT_SALT_LEN + 24 + 16,
        _ => return Err("Unsupported vault version".into()),
    };
    if data.len() < minimum {
        return Err("Truncated RTerm vault".into());
    }
    Ok(version)
}

fn read_vault_backup(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Open vault: {e}"))?;
    if !metadata.is_file() || metadata.len() > MAX_VAULT_BACKUP {
        return Err("Vault backup must be a regular file no larger than 128 MiB".into());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("Open vault: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Vault backup must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_VAULT_BACKUP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Read vault: {e}"))?;
    if bytes.len() as u64 > MAX_VAULT_BACKUP {
        return Err("Vault backup exceeds 128 MiB".into());
    }
    vault_version(&bytes)?;
    Ok(bytes)
}

pub(crate) fn read_private_config_file(
    path: &Path,
    max_bytes: u64,
) -> Result<Option<Vec<u8>>, String> {
    let directory = path.parent().ok_or("Missing config directory")?;
    if !ensure_private_config_dir(directory) {
        return Err("Unsafe RTerm config directory".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err("Config file must be a regular file owned only by this user".into());
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        if metadata.len() > max_bytes {
            return Err("Config file exceeds its size limit".into());
        }
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > max_bytes {
            return Err("Config file exceeds its size limit".into());
        }
        Ok(Some(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        options.custom_flags(0x00200000);
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if !metadata.is_file()
            || metadata.file_attributes() & 0x400 != 0
            || info.nNumberOfLinks != 1
            || !windows_file_is_private(&file)
        {
            return Err("Config file must be a private regular file".into());
        }
        if metadata.file_size() > max_bytes {
            return Err("Config file exceeds its size limit".into());
        }
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > max_bytes {
            return Err("Config file exceeds its size limit".into());
        }
        Ok(Some(bytes))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("Config file must be a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > max_bytes {
            return Err("Config file exceeds its size limit".into());
        }
        Ok(Some(bytes))
    }
}

fn read_private_vault(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = read_private_config_file(path, MAX_VAULT_BACKUP)?
        .ok_or_else(|| "Vault file not found".to_string())?;
    vault_version(&bytes)?;
    Ok(bytes)
}

fn write_new_backup(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("Missing backup directory")?;
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist_noclobber(path)
        .map_err(|e| format!("Cannot create vault backup (choose a new filename): {e}"))?;
    Ok(())
}

pub(crate) fn export_vault(destination: &Path, key: &VaultKey) -> Result<(), String> {
    export_vault_at(&get_config_dir().join("vault.dat"), destination, key)
}

fn export_vault_at(path: &Path, destination: &Path, key: &VaultKey) -> Result<(), String> {
    let _lock = lock_storage(path)?;
    let bytes = read_private_vault(path)?;
    decode_vault(&bytes, key)?;
    write_new_backup(destination, &bytes)
}

pub(crate) struct VaultBackup {
    bytes: Vec<u8>,
}

impl VaultBackup {
    pub(crate) fn read(path: &Path, password: &str) -> Result<Self, String> {
        let bytes = read_vault_backup(path)?;
        let (key, _) = VaultKey::from_data(password, &bytes)?;
        decode_vault(&bytes, &key)?;
        Ok(Self { bytes })
    }
}

pub(crate) fn import_vault(
    backup: VaultBackup,
    current_key: Option<&VaultKey>,
    replace: bool,
) -> Result<Option<PathBuf>, String> {
    import_vault_at(&get_config_dir(), backup, current_key, replace)
}

fn import_vault_at(
    directory: &Path,
    backup: VaultBackup,
    current_key: Option<&VaultKey>,
    replace: bool,
) -> Result<Option<PathBuf>, String> {
    let path = directory.join("vault.dat");
    let _lock = lock_storage(&path)?;
    let previous = if path.exists() {
        if !replace {
            return Err("An existing vault must be explicitly replaced".into());
        }
        let key = current_key.ok_or("Unlock the existing vault before replacing it")?;
        let bytes = read_private_vault(&path)?;
        decode_vault(&bytes, key)?;
        let mut file = tempfile::Builder::new()
            .prefix("vault-before-import-")
            .suffix(".rterm-vault")
            .tempfile_in(directory)
            .map_err(|e| e.to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        let (_, saved) = file.keep().map_err(|e| e.to_string())?;
        Some((bytes, saved))
    } else {
        None
    };
    write_atomic(&path, &backup.bytes)?;
    if let Err(error) = save_settings_at(
        &directory.join("settings.json"),
        &[
            ("vault_enabled", "1".into()),
            ("ai_tunnel_autoconnect", "0".into()),
        ],
    ) {
        let rollback = match &previous {
            Some((bytes, _)) => write_atomic(&path, bytes),
            None => std::fs::remove_file(&path).map_err(|e| e.to_string()),
        };
        rollback.map_err(|e| format!("Vault settings failed: {error}; rollback failed: {e}"))?;
        return Err(error);
    }
    Ok(previous.map(|(_, saved)| saved))
}

pub(crate) fn save_vault(
    sessions_data: &[Profile],
    session_folders: &BTreeSet<String>,
    keys_data: &[SshKey],
    key: &VaultKey,
    create: bool,
) -> Result<(), String> {
    update_vault(&get_config_dir().join("vault.dat"), key, create, |vault| {
        vault.profiles = sessions_data.to_vec();
        vault.session_folders.clone_from(session_folders);
        vault.keys = keys_data.to_vec();
    })?;
    Ok(())
}

pub(crate) fn save_agent_audit(
    key: &VaultKey,
    entries: impl Iterator<Item = AgentAuditEntry>,
) -> Result<VecDeque<AgentAuditEntry>, String> {
    save_agent_audit_at(&get_config_dir().join("vault.dat"), key, entries)
}

fn save_agent_audit_at(
    path: &Path,
    key: &VaultKey,
    entries: impl Iterator<Item = AgentAuditEntry>,
) -> Result<VecDeque<AgentAuditEntry>, String> {
    update_vault(path, key, false, |vault| {
        // Merge by ID in one pass rather than scanning the entire history
        // for each changed command.
        let mut updates: std::collections::HashMap<_, _> =
            entries.map(|entry| (entry.id.clone(), entry)).collect();
        for existing in &mut vault.agent_audit {
            if let Some(entry) = updates.remove(&existing.id) {
                *existing = entry;
            }
        }
        vault.agent_audit.extend(updates.into_values());
        vault
            .agent_audit
            .make_contiguous()
            .sort_by_key(|entry| std::cmp::Reverse(entry.submitted_ms));
    })
    .map(|vault| vault.agent_audit)
}

// Delete only the explicitly reviewed audit IDs. Use the same vault lock
// and atomic replacement as other writers, preserving credentials and keys.
pub(crate) fn delete_agent_audit(
    key: &VaultKey,
    ids: &BTreeSet<String>,
) -> Result<VecDeque<AgentAuditEntry>, String> {
    delete_agent_audit_at(&get_config_dir().join("vault.dat"), key, ids)
}

fn delete_agent_audit_at(
    path: &Path,
    key: &VaultKey,
    ids: &BTreeSet<String>,
) -> Result<VecDeque<AgentAuditEntry>, String> {
    update_vault(path, key, false, |vault| {
        vault.agent_audit.retain(|entry| !ids.contains(&entry.id));
    })
    .map(|vault| vault.agent_audit)
}

pub(crate) fn save_openai_tunnel(
    key: &VaultKey,
    profile: Option<crate::tunnel::Profile>,
) -> Result<(), String> {
    update_vault(&get_config_dir().join("vault.dat"), key, false, |vault| {
        vault.openai_tunnel = profile;
    })?;
    Ok(())
}

fn update_vault(
    path: &Path,
    key: &VaultKey,
    create: bool,
    update: impl FnOnce(&mut Vault),
) -> Result<Vault, String> {
    // Vault writers replace the data file atomically. Lock a separate file so
    // another RTerm window cannot replace profiles or audit history mid-write.
    let _lock = lock_storage(path)?;
    let mut vault = if create {
        if path.exists() {
            return Err("Vault already exists. Unlock it before saving.".into());
        }
        Vault::default()
    } else {
        decode_vault(&read_private_vault(path)?, key)?
    };
    update(&mut vault);
    let bytes = encode_vault(&vault, key)?;
    write_atomic(path, &bytes)?;
    Ok(vault)
}

pub(crate) fn lock_storage(path: &Path) -> Result<std::fs::File, String> {
    let directory = path.parent().ok_or("Missing storage directory")?;
    if !ensure_private_config_dir(directory) {
        return Err("Unsafe RTerm storage directory".into());
    }
    let lock_path = path.with_extension("lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let file = options
            .open(&lock_path)
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err("Unsafe RTerm storage lock file".into());
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
        }
        file.lock().map_err(|error| error.to_string())?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::{
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        options.custom_flags(0x00200000).share_mode(3);
        let file = options
            .open(&lock_path)
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if !metadata.is_file()
            || metadata.file_attributes() & 0x400 != 0
            || info.nNumberOfLinks != 1
            || !windows_file_is_private(&file)
        {
            return Err("Unsafe RTerm storage lock file".into());
        }
        file.lock().map_err(|error| error.to_string())?;
        Ok(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let file = options
            .open(&lock_path)
            .map_err(|error| error.to_string())?;
        if !file
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err("Unsafe RTerm storage lock file".into());
        }
        file.lock().map_err(|error| error.to_string())?;
        Ok(file)
    }
}

fn encode_vault(vault: &Vault, key: &VaultKey) -> Result<Vec<u8>, String> {
    // Keep the old SSH session records so the vault can still be opened by Wry builds.
    let legacy = vault
        .profiles
        .iter()
        .filter_map(|profile| match &profile.connection {
            Connection::Ssh { config, .. } => {
                let mut config = config.clone();
                config.name = Some(profile.name.clone());
                config.vault_pass = None;
                Some(config)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    #[derive(serde::Serialize)]
    struct Container<'a> {
        v: u8,
        sessions: &'a [SshConfig],
        profiles: &'a [Profile],
        session_folders: &'a BTreeSet<String>,
        keys: &'a [SshKey],
        agent_audit: &'a VecDeque<AgentAuditEntry>,
        openai_tunnel: &'a Option<crate::tunnel::Profile>,
    }
    let container = Container {
        v: 2,
        sessions: &legacy,
        profiles: &vault.profiles,
        session_folders: &vault.session_folders,
        keys: &vault.keys,
        agent_audit: &vault.agent_audit,
        openai_tunnel: &vault.openai_tunnel,
    };
    let json = zeroize::Zeroizing::new(serde_json::to_vec(&container).map_err(|e| e.to_string())?);
    // Header + salt + nonce + authentication tag. Never write a vault
    // that its own reader or backup importer would refuse to reopen.
    const OVERHEAD: usize = 6 + VAULT_SALT_LEN + 24 + 16;
    if json.len() as u64 > MAX_VAULT_BACKUP - OVERHEAD as u64 {
        return Err("Encrypted vault exceeds 128 MiB; existing vault was not changed".into());
    }
    let cipher = XChaCha20Poly1305::new_from_slice(&key.0).unwrap();
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, json.as_slice())
        .map_err(|e| format!("encrypt error: {:?}", e))?;
    let mut buf = Vec::new();
    buf.extend_from_slice(VAULT_MAGIC);
    buf.extend_from_slice(&VAULT_VERSION.to_le_bytes());
    buf.extend_from_slice(&key.1);
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&ciphertext);
    Ok(buf)
}

pub(crate) fn load_vault(password: &str) -> Result<(Vault, VaultKey), String> {
    load_vault_at(&get_config_dir().join("vault.dat"), password)
}

fn load_vault_at(path: &Path, password: &str) -> Result<(Vault, VaultKey), String> {
    let _lock = lock_storage(path)?;
    let data = read_private_vault(path)?;
    let (key, legacy) = VaultKey::from_data(password, &data)?;
    let vault = decode_vault(&data, &key)?;
    if legacy {
        // Upgrade the shared-salt format only after successful authentication.
        let key = VaultKey::derive(password);
        write_atomic(path, &encode_vault(&vault, &key)?)?;
        Ok((vault, key))
    } else {
        Ok((vault, key))
    }
}

fn decode_vault(data: &[u8], key: &VaultKey) -> Result<Vault, String> {
    let (nonce, ciphertext) =
        match vault_version(data).map_err(|_| "wrong password or corrupted vault".to_string())? {
            LEGACY_VAULT_VERSION => (XNonce::from_slice(&data[6..30]), &data[30..]),
            VAULT_VERSION => (
                XNonce::from_slice(&data[6 + VAULT_SALT_LEN..6 + VAULT_SALT_LEN + 24]),
                &data[6 + VAULT_SALT_LEN + 24..],
            ),
            _ => unreachable!(),
        };
    let cipher = XChaCha20Poly1305::new_from_slice(&key.0).unwrap();
    match cipher.decrypt(nonce, ciphertext) {
        Ok(plaintext) => {
            let plaintext = zeroize::Zeroizing::new(plaintext);
            let mut container: serde_json::Value =
                serde_json::from_slice(&plaintext).map_err(|_| "invalid json".to_string())?;
            let mut sessions: Vec<Profile> = match container
                .as_object_mut()
                .and_then(|map| map.remove("profiles").or_else(|| map.remove("sessions")))
            {
                Some(serde_json::Value::Array(sessions)) => sessions
                    .into_iter()
                    .filter_map(|session| {
                        if session.get("connection").is_some() {
                            serde_json::from_value::<Profile>(session).ok()
                        } else {
                            serde_json::from_value::<SshConfig>(session)
                                .ok()
                                .map(Profile::from)
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let keys: Vec<SshKey> = container
                .as_object_mut()
                .and_then(|map| map.remove("keys"))
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            let agent_audit: VecDeque<AgentAuditEntry> = match container
                .as_object_mut()
                .and_then(|map| map.remove("agent_audit"))
            {
                Some(serde_json::Value::Array(entries)) => entries
                    .into_iter()
                    .filter_map(|entry| serde_json::from_value(entry).ok())
                    .collect(),
                _ => VecDeque::new(),
            };
            let openai_tunnel = container
                .as_object_mut()
                .and_then(|map| map.remove("openai_tunnel"))
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "Invalid OpenAI tunnel profile")?
                .flatten();
            let mut session_folders: BTreeSet<String> = container
                .as_object_mut()
                .and_then(|map| map.remove("session_folders"))
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default();
            normalize_session_folders(&mut sessions, &mut session_folders);
            Ok(Vault {
                profiles: sessions,
                session_folders,
                keys,
                agent_audit,
                openai_tunnel,
            })
        }
        Err(_) => Err("wrong password".to_string()),
    }
}

fn get_settings_path() -> std::path::PathBuf {
    get_config_dir().join("settings.json")
}

pub(crate) fn save_settings(entries: &[(&str, String)]) -> Result<(), String> {
    save_settings_at(&get_settings_path(), entries)
}

fn save_settings_at(path: &Path, entries: &[(&str, String)]) -> Result<(), String> {
    // Serialize read-modify-write operations and never replace a corrupt or
    // unreadable settings file with an empty object.
    let _lock = lock_storage(path)?;
    let mut settings: serde_json::Map<String, serde_json::Value> =
        match read_private_config_file(path, MAX_CONFIG_FILE)? {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|error| format!("Read settings: {error}"))?
            }
            None => Default::default(),
        };
    settings.extend(
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), serde_json::Value::String(value.clone()))),
    );
    write_atomic(
        path,
        serde_json::Value::Object(settings).to_string().as_bytes(),
    )
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let directory = path.parent().ok_or("Missing destination directory")?;
    if !ensure_private_config_dir(directory) {
        return Err("Unsafe RTerm storage directory".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if !windows_file_is_private(file.as_file()) {
        return Err("Config file permissions are not private".into());
    }
    file.write_all(bytes)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| error.to_string())?;
    file.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn private_temp_dir() -> Result<PathBuf, String> {
    let directory = get_config_dir().join("tmp");
    if ensure_private_config_dir(&directory) {
        Ok(directory)
    } else {
        Err("Unsafe RTerm temporary directory".into())
    }
}

fn private_temp_file_in(
    directory: &Path,
    prefix: &str,
    suffix: &str,
) -> std::io::Result<tempfile::NamedTempFile> {
    let file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile_in(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let metadata = file.as_file().metadata()?;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Temporary file is not private",
            ));
        }
    }
    #[cfg(windows)]
    if !windows_file_is_private(file.as_file()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Temporary file permissions are not private",
        ));
    }
    Ok(file)
}

pub(crate) fn create_private_temp_file(
    prefix: &str,
    suffix: &str,
) -> std::io::Result<tempfile::NamedTempFile> {
    let directory = private_temp_dir()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::PermissionDenied, error))?;
    private_temp_file_in(&directory, prefix, suffix)
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SavedSessions {
    pub(crate) profiles: Vec<Profile>,
    #[serde(default)]
    pub(crate) folders: BTreeSet<String>,
}

pub(crate) fn valid_session_folder(name: &str) -> bool {
    crate::agent_protocol::valid_library_name(name)
}

fn normalize_session_folders(profiles: &mut [Profile], folders: &mut BTreeSet<String>) {
    folders.retain(|name| valid_session_folder(name));
    for profile in profiles {
        if let Some(folder) = &profile.folder {
            if valid_session_folder(folder) {
                folders.insert(folder.clone());
            } else {
                profile.folder = None;
            }
        }
    }
}

fn decode_saved_sessions(bytes: &[u8]) -> Result<SavedSessions, String> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Format {
        Library(SavedSessions),
        Legacy(Vec<Profile>),
    }
    let mut library = match serde_json::from_slice(bytes).map_err(|error| error.to_string())? {
        Format::Library(library) => library,
        Format::Legacy(profiles) => SavedSessions {
            profiles,
            folders: Default::default(),
        },
    };
    normalize_session_folders(&mut library.profiles, &mut library.folders);
    Ok(library)
}

pub(crate) fn load_profiles() -> SavedSessions {
    read_private_config_file(&get_config_dir().join("sessions.json"), MAX_CONFIG_FILE)
        .ok()
        .flatten()
        .and_then(|bytes| decode_saved_sessions(&bytes).ok())
        .unwrap_or_default()
}

fn encode_saved_sessions(
    profiles: &[Profile],
    folders: &BTreeSet<String>,
) -> Result<Vec<u8>, String> {
    let mut library = SavedSessions {
        profiles: profiles.iter().map(Profile::without_credentials).collect(),
        folders: folders.clone(),
    };
    normalize_session_folders(&mut library.profiles, &mut library.folders);
    // Preserve the legacy array format until folder metadata is actually used.
    if library.folders.is_empty() {
        serde_json::to_vec(&library.profiles)
    } else {
        serde_json::to_vec(&library)
    }
    .map_err(|error| error.to_string())
}

pub(crate) fn save_profiles(
    profiles: &[Profile],
    folders: &BTreeSet<String>,
) -> Result<(), String> {
    let bytes = encode_saved_sessions(profiles, folders)?;
    write_atomic(&get_config_dir().join("sessions.json"), &bytes)
}

pub(crate) fn import_vault_profiles(
    key: &VaultKey,
    incoming: Vec<Profile>,
) -> Result<(SavedSessions, usize), String> {
    import_vault_profiles_at(&get_config_dir().join("vault.dat"), key, incoming)
}

fn import_vault_profiles_at(
    path: &Path,
    key: &VaultKey,
    incoming: Vec<Profile>,
) -> Result<(SavedSessions, usize), String> {
    let mut added = 0;
    let vault = update_vault(path, key, false, |vault| {
        added = crate::session_import::append_unique(&mut vault.profiles, incoming);
    })?;
    Ok((
        SavedSessions {
            profiles: vault.profiles,
            folders: vault.session_folders,
        },
        added,
    ))
}

pub(crate) fn delete_vault() -> Result<(), String> {
    let path = get_config_dir().join("vault.dat");
    let _lock = lock_storage(&path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn load_setting(key: &str) -> Option<String> {
    let path = get_settings_path();
    let bytes = read_private_config_file(&path, MAX_CONFIG_FILE).ok()??;
    let settings: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    settings
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn ssh_dir() -> std::path::PathBuf {
    home_dir().join(".ssh")
}

pub(crate) fn make_key(
    name: &str,
    algo: &str,
    passphrase: &str,
    comment: &str,
) -> Result<SshKey, String> {
    use ssh_key::{
        Algorithm, EcdsaCurve, PrivateKey,
        private::{KeypairData, RsaKeypair},
    };
    let name = crate::backend::safe_name(name)?;
    let mut rng = rand::rng();
    let mut key = match algo {
        "rsa-2048" | "rsa-4096" => {
            let bits = if algo == "rsa-2048" { 2048 } else { 4096 };
            let key = RsaKeypair::random(&mut rng, bits).map_err(|error| error.to_string())?;
            PrivateKey::new(KeypairData::Rsa(key), comment)
        }
        _ => {
            let algorithm = match algo {
                "ecdsa-p256" => Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256,
                },
                "ecdsa-p384" => Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP384,
                },
                "ecdsa-p521" => Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP521,
                },
                _ => Algorithm::Ed25519,
            };
            PrivateKey::random(&mut rng, algorithm)
        }
    }
    .map_err(|error| error.to_string())?;
    key.set_comment(comment);
    let public_key = key
        .public_key()
        .to_openssh()
        .map_err(|error| error.to_string())?;
    let encoded = if passphrase.is_empty() {
        key
    } else {
        key.encrypt(&mut rng, passphrase)
            .map_err(|error| error.to_string())?
    };
    let private_key = encoded
        .to_openssh(ssh_key::LineEnding::LF)
        .map_err(|error| error.to_string())?
        .to_string();
    Ok(SshKey {
        name: name.into(),
        public_key,
        private_key,
        key_type: algo.into(),
    })
}

pub(crate) fn parse_key(name: &str, private: &str, public: &str) -> Result<SshKey, String> {
    let name = crate::backend::safe_name(name)?;
    let private = private.trim();
    let public = public.trim();
    let derived = ssh_key::PrivateKey::from_openssh(private)
        .map(|key| key.public_key().clone())
        .or_else(|_| {
            russh::keys::decode_secret_key(private, None).map(|key| key.public_key().clone())
        })
        .map_err(|error| format!("Could not read the private key: {error}"))?;
    let key = if public.is_empty() {
        derived
    } else {
        let supplied =
            ssh_key::PublicKey::from_openssh(public).map_err(|error| error.to_string())?;
        if supplied.key_data() != derived.key_data() {
            return Err("The public key does not match the private key".into());
        }
        supplied
    };
    let public_key = key.to_openssh().map_err(|error| error.to_string())?;
    Ok(SshKey {
        name: name.into(),
        private_key: format!("{private}\n"),
        public_key,
        key_type: key.algorithm().to_string(),
    })
}

pub(crate) fn write_key(key: &SshKey) -> Result<(), String> {
    let directory = ssh_dir();
    if !ensure_private_config_dir(&directory) {
        return Err("Unsafe SSH key directory".into());
    }
    let name = crate::backend::safe_name(&key.name)?;
    let private = directory.join(name);
    let public = directory.join(format!("{name}.pub"));
    let mut private_file =
        tempfile::NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
    let mut public_file =
        tempfile::NamedTempFile::new_in(&directory).map_err(|error| error.to_string())?;
    private_file
        .write_all(key.private_key.as_bytes())
        .map_err(|error| error.to_string())?;
    writeln!(public_file, "{}", key.public_key).map_err(|error| error.to_string())?;
    private_file
        .persist_noclobber(&private)
        .map_err(|error| error.to_string())?;
    if let Err(error) = public_file.persist_noclobber(&public) {
        let _ = std::fs::remove_file(&private);
        return Err(error.to_string());
    }
    Ok(())
}

pub(crate) fn list_keys() -> Vec<serde_json::Value> {
    let dir = ssh_dir();
    let mut keys = Vec::new();
    if !ensure_private_config_dir(&dir) {
        return keys;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return keys,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".pub")
            || name.starts_with('.')
            || entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
        {
            continue;
        }
        let content = match read_private_config_file(&path, MAX_CONFIG_FILE)
            .ok()
            .flatten()
            .and_then(|bytes| String::from_utf8(bytes).ok())
        {
            Some(content) => content,
            None => continue,
        };
        if !content.contains("BEGIN") || !content.contains("PRIVATE KEY") {
            continue;
        }
        let pub_path = dir.join(format!("{}.pub", name));
        let public_key = std::fs::read_to_string(&pub_path)
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let ktype = if content.contains("OPENSSH") || public_key.contains("ssh-ed25519") {
            "ed25519"
        } else if content.contains("RSA") || public_key.contains("ssh-rsa") {
            "rsa"
        } else if content.contains("EC") || public_key.contains("ecdsa") {
            "ecdsa"
        } else {
            "unknown"
        };
        keys.push(serde_json::json!({
            "name": name,
            "path": path.to_string_lossy().to_string(),
            "type": ktype,
            "public_key": public_key,
        }));
    }
    keys.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    keys
}

pub(crate) fn delete_key(path: &str) -> Result<(), String> {
    let path = PathBuf::from(path);
    let directory = ssh_dir();
    if path.parent() != Some(directory.as_path()) {
        return Err("SSH key is outside the key directory".into());
    }
    if !ensure_private_config_dir(&directory) {
        return Err("Unsafe SSH key directory".into());
    }
    std::fs::remove_file(&path).map_err(|error| error.to_string())?;
    let mut public = path.into_os_string();
    public.push(".pub");
    match std::fs::remove_file(public) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
pub(crate) fn remove_profiles() -> Result<(), String> {
    let path = get_config_dir().join("sessions.json");
    let _lock = lock_storage(&path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
#[path = "../tests/unit/storage.rs"]
mod tests;

#[cfg(test)]
pub(crate) use tests::test_private_tempdir;
