//! Per-user desktop shortcut setup for installed Unix builds. System
//! package managers must not guess a graphical user's home while elevated.
#[cfg(target_os = "linux")]
use std::path::Path;
use std::{fs, io, path::PathBuf};

pub(crate) struct Setup {
    executable: PathBuf,
    destination: PathBuf,
    decision: PathBuf,
}

impl Setup {
    pub(crate) fn discover() -> Option<Self> {
        if unsafe { libc::geteuid() } == 0 {
            return None;
        }
        let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
        let bin = executable.parent()?;
        if executable.file_name()?.to_str()? != "rterm" || !bin.join("rterm-agent").is_file() {
            return None;
        }
        // Only installed builds offer setup, never cargo run or test binaries.
        #[cfg(target_os = "macos")]
        let executable = {
            let contents = bin.parent()?;
            let bundle = contents.parent()?;
            if bin.file_name()?.to_str()? != "MacOS"
                || contents.file_name()?.to_str()? != "Contents"
                || bundle.extension()?.to_str()? != "app"
                || !contents.join("Info.plist").is_file()
            {
                return None;
            }
            bundle.to_path_buf()
        };
        #[cfg(target_os = "linux")]
        if !bin
            .parent()?
            .join("share/applications/rterm.desktop")
            .is_file()
        {
            return None;
        }
        let home = PathBuf::from(std::env::var_os("HOME")?);
        if !home.is_absolute() {
            return None;
        }
        #[cfg(target_os = "macos")]
        let desktop = home.join("Desktop");
        #[cfg(target_os = "linux")]
        let desktop = linux_desktop(&home);
        if !desktop.is_absolute() || desktop == home || !desktop.is_dir() {
            return None;
        }
        #[cfg(target_os = "macos")]
        let destination = desktop.join("RTerm.app");
        #[cfg(target_os = "linux")]
        let destination = desktop.join("RTerm.desktop");
        Some(Self {
            executable,
            destination,
            decision: crate::storage::get_config_dir().join("desktop-shortcut-choice-v1"),
        })
    }

    pub(crate) fn pending(&self) -> bool {
        !self.decision.exists()
    }

    pub(crate) fn choose(&self, create: bool) -> io::Result<()> {
        if create {
            self.create()?;
        }
        // Keep this separate from settings and the encrypted vault. Never
        // overwrite an existing choice or follow a marker symlink.
        fs::create_dir_all(
            self.decision
                .parent()
                .ok_or_else(|| io::Error::other("No config directory"))?,
        )?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.decision)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(if create { b"yes\n" } else { b"no\n" })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    }

    #[cfg(target_os = "macos")]
    fn create(&self) -> io::Result<()> {
        match std::os::unix::fs::symlink(&self.executable, &self.destination) {
            Ok(()) => Ok(()),
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && fs::read_link(&self.destination).ok().as_ref() == Some(&self.executable) =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(target_os = "linux")]
    fn create(&self) -> io::Result<()> {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let entry = desktop_entry(&self.executable)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&self.destination)
        {
            Ok(mut file) => file.write_all(entry.as_bytes()),
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && fs::symlink_metadata(&self.destination).is_ok_and(|m| m.is_file())
                    && fs::read_to_string(&self.destination).ok().as_deref() == Some(&entry) =>
            {
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_desktop(home: &Path) -> PathBuf {
    crate::tunnel::find_trusted_executable("xdg-user-dir")
        .and_then(|client| {
            std::process::Command::new(client)
                .arg("DESKTOP")
                .output()
                .ok()
        })
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|out| PathBuf::from(out.trim_end_matches(['\r', '\n'])))
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join("Desktop"))
}

#[cfg(target_os = "linux")]
fn desktop_entry(executable: &Path) -> io::Result<String> {
    let path = executable
        .to_str()
        .filter(|s| !s.chars().any(char::is_control))
        .ok_or_else(|| {
            io::Error::other("Desktop shortcut requires a Unicode path without control characters")
        })?;
    // Exec quoting has two levels of escaping: argument quoting, then
    // Desktop Entry string escaping. Percent signs must not be field codes.
    let mut arg = String::new();
    for ch in path.chars() {
        match ch {
            '%' => arg.push_str("%%"),
            '"' | '`' | '$' | '\\' => {
                arg.push('\\');
                arg.push(ch);
            }
            _ => arg.push(ch),
        }
    }
    let arg = arg.replace('\\', "\\\\");
    // Match the installer: GLib checks the executable before expanding
    // percent escapes, and '=' is not allowed in an executable token.
    // The path is a positional argument, not interpolated shell code.
    let exec = if path.contains(['%', '=']) {
        format!(r#"/bin/sh -c "exec \\"\\$0\\"" "{arg}""#)
    } else {
        format!("\"{arg}\"")
    };
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=RTerm\nGenericName=Terminal Emulator\nComment=Native terminal and SSH client\nExec={exec}\nIcon=rterm\nTerminal=false\nCategories=System;TerminalEmulator;\nKeywords=Terminal;SSH;SFTP;Shell;Console;\nStartupWMClass=rterm\n"
    ))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../tests/unit/desktop_shortcut.rs"]
mod tests;

#[cfg(all(test, target_os = "macos"))]
#[path = "../tests/unit/desktop_shortcut_mac_tests.rs"]
mod mac_tests;
