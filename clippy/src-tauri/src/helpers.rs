/// Strip the directory from a path so logs never contain the user's home
/// directory or other path components. Returns the filename only.
pub(crate) fn basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// Truncate a string to `max` bytes for log entries, appending "…" if cut.
pub(crate) fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}…", &s[..max]) }
}

/// Escape a path for ffmpeg's concat-demuxer text format ("file '...'").
/// Rejects paths containing newline/CR — without this, a crafted filename
/// like `evil\nfile '/etc/passwd'` would let the attacker inject additional
/// concat directives and exfiltrate or substitute files into the output.
pub(crate) fn escape_concat_path(p: &str) -> Result<String, String> {
    if p.contains('\n') || p.contains('\r') {
        return Err("path contains newline/CR characters".into());
    }
    Ok(p.replace('\\', "/").replace('\'', "'\\''"))
}
