//! Shared CLI/server validation. Paths are explicit, never shell-expanded.
pub fn validate_paths(local: &str, remote: &str) -> Result<(), &'static str> {
    let safe = |path: &str| {
        !path.is_empty()
            && path.len() <= 4096
            && !path.chars().any(|c| {
                c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
    };
    #[cfg(windows)]
    let bytes = local.as_bytes();
    #[cfg(windows)]
    if !safe(local)
        || bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
        || local[3..].split(['\\', '/']).any(|part| {
            part.is_empty() || part == "." || part == ".." || invalid_windows_name(part)
        })
    {
        return Err(
            "Use an absolute Windows drive path, without device names, streams, or traversal",
        );
    }
    #[cfg(unix)]
    if !safe(local)
        || !local.starts_with('/')
        || local[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(
            "Use an absolute local file or folder path, without traversal or a trailing slash",
        );
    }
    if !safe(remote)
        || !remote.starts_with('/')
        || remote[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("Use an absolute remote path, without traversal or a trailing slash");
    }
    Ok(())
}

pub(crate) fn invalid_windows_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    name.ends_with(['.', ' '])
        || name.contains([':', '*', '?', '"', '<', '>', '|'])
        || matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}
