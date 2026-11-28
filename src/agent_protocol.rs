//! Shared wire limits and validation for the desktop, CLI and IPC transports.
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAX_REQUEST: usize = 512 * 1024;
pub const MAX_SCRIPT: usize = 64 * 1024;
pub const MAX_RESPONSE: usize = 1024 * 1024;
pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_WAIT_MS: u64 = 30_000;
pub const DEFAULT_WAIT_MS: u64 = 1_000;
pub const MAX_COMMAND_SECS: u64 = 3600;
pub const MAX_JOBS: usize = 32;

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn validate_command(command: &str) -> Result<(), &'static str> {
    if command.trim().is_empty() || command.len() > MAX_SCRIPT || command.contains('\0') {
        return Err("Command must contain 1-65536 UTF-8 bytes, without NUL");
    }
    Ok(())
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Vec<u8>> {
    let n = reader.read_u32_le().await? as usize;
    if n == 0 || n > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid IPC frame size",
        ));
    }
    let mut bytes = vec![0; n];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    bytes: &[u8],
    max: usize,
) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > max || u32::try_from(bytes.len()).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid IPC frame size",
        ));
    }
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    writer.write_all(&frame).await?;
    writer.flush().await
}

pub(crate) fn valid_library_name(name: &str) -> bool {
    !name.is_empty()
        && name == name.trim()
        && name.chars().count() <= 128
        && !name.chars().any(|ch| ch.is_control() || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

// A move must explicitly specify either a folder name or null (ungrouped).
fn required_nullable_folder<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    serde::Deserialize::deserialize(d)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LibraryChange {
    CreateFolder {
        name: String,
    },
    RenameFolder {
        folder: String,
        name: String,
    },
    RemoveFolder {
        folder: String,
    },
    MoveSession {
        profile_id: String,
        #[serde(deserialize_with = "required_nullable_folder")]
        folder: Option<String>,
    },
    RenameSession {
        profile_id: String,
        name: String,
    },
    SetPort {
        profile_id: String,
        port: u16,
    },
}

impl LibraryChange {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        let name = |value: &str| {
            if valid_library_name(value) {
                Ok(())
            } else {
                Err(
                    "Invalid name: use 1–128 characters without surrounding whitespace or control characters",
                )
            }
        };
        let id = |value: &str| {
            if valid_id(value) {
                Ok(())
            } else {
                Err("Invalid profile ID")
            }
        };
        match self {
            Self::CreateFolder { name: value } => name(value),
            Self::RenameFolder {
                folder,
                name: value,
            } => {
                name(folder)?;
                name(value)
            }
            Self::RemoveFolder { folder } => name(folder),
            Self::MoveSession { profile_id, folder } => {
                id(profile_id)?;
                if let Some(folder) = folder {
                    name(folder)?;
                }
                Ok(())
            }
            Self::RenameSession {
                profile_id,
                name: value,
            } => {
                id(profile_id)?;
                name(value)
            }
            Self::SetPort { profile_id, port } => {
                id(profile_id)?;
                if *port == 0 {
                    Err("Port must be 1 through 65535")
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/agent_protocol.rs"]
mod tests;
