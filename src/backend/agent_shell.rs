//! Foreground AI execution on the user's existing SSH PTY.
//! Commands use the existing shell. The app applies its approval policy and
//! checks the terminal before dispatch.
use crate::agent::{self, Permit, Reply};
use serde_json::{Value, json};
use std::sync::mpsc;

use super::{BackendResult, Reply as BackendReply};

// Stay below common 4096-byte canonical terminal line limits, including
// the wrapper and newline. Large scripts should be uploaded using SFTP.
const MAX_LINE: usize = 3500;

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn frame(command: &str, nonce: &str) -> Result<Vec<u8>, String> {
    let command = command.replace("\r\n", "\n");
    if command
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return Err("Foreground commands cannot contain terminal control characters".into());
    }
    let script = if command.contains(['\n', '\t']) {
        let escaped = command
            .replace('\\', "\\\\")
            .replace('\n', "\\0012")
            .replace('\t', "\\0011");
        // Preserve trailing newlines through POSIX command substitution.
        let var = format!("__rterm_{nonce}");
        format!(
            "{var}=$(command printf '%b' {}; command printf '.'); eval \"unset {var}; ${{{var}%.}}\"",
            quote(&escaped)
        )
    } else {
        format!("eval {}", quote(&command))
    };
    // The real shell echoes this line. Private OSC markers are ignored by the
    // terminal emulator and delimit the CLI capture, not the terminal display.
    let line = format!(
        "command printf '\\033]777;rt-{nonce};B\\007'; {script}; command printf '\\033]777;rt-{nonce};E;%s\\007' \"$?\"\r"
    );
    if line.len() > MAX_LINE {
        return Err("Foreground command is too long for a terminal input line; use SFTP to upload a script, or enable background execution".into());
    }
    Ok(line.into_bytes())
}

pub(crate) fn validate(command: &str) -> Result<(), String> {
    frame(command, "00000000000000000000000000000000").map(|_| ())
}

pub(super) struct ShellJob {
    pub(super) line: Vec<u8>,
    pub(super) permit: Permit,
    result_reply: Reply,
    response: mpsc::Sender<BackendResult>,
    begin: Vec<u8>,
    end: Vec<u8>,
    pending: Vec<u8>,
    started: bool,
    preview: Vec<u8>,
    truncated: bool,
    done: bool,
}

impl ShellJob {
    pub(super) fn new(
        command: &str,
        permit: Permit,
        result_reply: Reply,
        response: mpsc::Sender<BackendResult>,
    ) -> Result<Self, String> {
        let nonce = agent::random_id();
        Ok(Self {
            line: frame(command, &nonce)?,
            permit,
            result_reply,
            response,
            begin: format!("\x1b]777;rt-{nonce};B\x07").into_bytes(),
            end: format!("\x1b]777;rt-{nonce};E;").into_bytes(),
            pending: Vec::new(),
            started: false,
            preview: Vec::new(),
            truncated: false,
            done: false,
        })
    }

    fn capture(preview: &mut Vec<u8>, truncated: &mut bool, result_reply: &Reply, bytes: &[u8]) {
        let count = bytes
            .len()
            .min(agent::MAX_OUTPUT.saturating_sub(preview.len()));
        preview.extend_from_slice(&bytes[..count]);
        *truncated |= count < bytes.len();
        result_reply.capture_output(agent::OutputStream::Combined, bytes);
    }

    // Return a result only after the actual interactive shell emits the
    // operation-specific end marker. Echoed input is never a start marker.
    pub(super) fn consume(&mut self, bytes: &[u8]) -> Option<Value> {
        self.pending.extend_from_slice(bytes);
        if !self.started {
            if let Some(pos) = find(&self.pending, &self.begin) {
                self.pending.drain(..pos + self.begin.len());
                self.started = true;
            } else {
                let keep = suffix(&self.pending, &self.begin);
                self.pending.drain(..self.pending.len() - keep);
                return None;
            }
        }
        if let Some(pos) = find(&self.pending, &self.end) {
            let tail = &self.pending[pos + self.end.len()..];
            if let Some(end) = tail.iter().position(|b| *b == 7) {
                let code = std::str::from_utf8(&tail[..end])
                    .ok()
                    .filter(|s| {
                        !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit())
                    })
                    .and_then(|s| s.parse::<u8>().ok());
                Self::capture(
                    &mut self.preview,
                    &mut self.truncated,
                    &self.result_reply,
                    &self.pending[..pos],
                );
                return Some(match code {
                    Some(code) => self.value(Some(code), None),
                    None => self.value(
                        None,
                        Some("Invalid shell completion marker; outcome uncertain"),
                    ),
                });
            }
            if tail.len() > 3 {
                return Some(self.value(
                    None,
                    Some("Invalid shell completion marker; outcome uncertain"),
                ));
            }
            Self::capture(
                &mut self.preview,
                &mut self.truncated,
                &self.result_reply,
                &self.pending[..pos],
            );
            self.pending.drain(..pos);
        } else {
            let keep = suffix(&self.pending, &self.end);
            let prefix_len = self.pending.len() - keep;
            Self::capture(
                &mut self.preview,
                &mut self.truncated,
                &self.result_reply,
                &self.pending[..prefix_len],
            );
            self.pending.drain(..prefix_len);
        }
        None
    }

    fn value(&self, code: Option<u8>, error: Option<&str>) -> Value {
        json!({"ok":code.is_some() && error.is_none(),"command_success":code == Some(0) && error.is_none(),
            "exit_code":code,"stdout":String::from_utf8_lossy(&self.preview),"stderr":"",
            "truncated":self.truncated,"error":error,"execution_mode":"interactive_shell","combined_output":true})
    }

    pub(super) fn fail(self, message: &str) {
        let value = self.value(None, Some(message));
        self.finish(value);
    }

    pub(super) fn require_reconnect(self) {
        let mut value = self.value(
            None,
            Some("Interactive command interrupted; remote effects may have occurred"),
        );
        value["shell_recovery_required"] = json!(true);
        self.finish(value);
    }

    pub(super) fn finish(mut self, value: Value) {
        self.done = true;
        let _ = self.result_reply.clone().send(value.clone());
        let _ = self.response.send(Ok(BackendReply::Agent(value)));
    }
}

impl Drop for ShellJob {
    fn drop(&mut self) {
        if !self.done {
            let value = self.value(
                None,
                Some("Interactive shell closed or command was not sent; outcome uncertain"),
            );
            let _ = self.result_reply.clone().send(value.clone());
            let _ = self.response.send(Ok(BackendReply::Agent(value)));
        }
    }
}

fn find(bytes: &[u8], pattern: &[u8]) -> Option<usize> {
    bytes
        .windows(pattern.len())
        .position(|window| window == pattern)
}

fn suffix(bytes: &[u8], pattern: &[u8]) -> usize {
    (1..=bytes.len().min(pattern.len() - 1))
        .rev()
        .find(|&n| bytes.ends_with(&pattern[..n]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_a_single_terminal_line_and_preserves_quotes() {
        let line = frame("cd '/tmp/a b'; printf \"%s\" 'hello'\n", "fixture").unwrap();
        let line = String::from_utf8(line).unwrap();
        assert!(line.ends_with('\r'));
        assert!(!line.contains('\n'));
        assert!(!line.contains('\x1b'));
        assert!(line.contains("eval"));
        assert!(line.contains("\\0012"));
        assert!(frame("bad\x03", "fixture").is_err());
        assert!(frame(&"x".repeat(3500), "fixture").is_err());
    }
}
