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

#[cfg(test)]
#[path = "../tests/unit/agent_protocol.rs"]
mod tests;
