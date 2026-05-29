//! Game allowlist + auto-detection.
//!
//! Replay only captures windows whose process executable is in the allowlist.
//! The list is seeded at session start by scanning local game launcher install
//! directories (Steam first), and the user can add games manually.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Default, Clone)]
pub struct GameAllowlist {
    /// Lowercased path strings for case-insensitive comparison on Windows.
    paths: HashSet<String>,
}

impl GameAllowlist {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, exe: &Path) {
        self.paths.insert(normalize(exe));
    }

    pub fn remove(&mut self, exe: &Path) -> bool {
        self.paths.remove(&normalize(exe))
    }

    pub fn contains(&self, exe: &Path) -> bool {
        self.paths.contains(&normalize(exe))
    }

    pub fn entries(&self) -> Vec<String> {
        let mut v: Vec<String> = self.paths.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn extend<I: IntoIterator<Item = PathBuf>>(&mut self, iter: I) {
        for p in iter {
            self.add(&p);
        }
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Canonical, lowercased, back-slashed string form of `path`.
///
/// Three-stage canonicalisation so `add("C:\Games\..\Sensitive\foo.exe")` and
/// `contains("C:\Sensitive\foo.exe")` agree on the same key:
///   1. `fs::canonicalize` when the file exists (resolves `..`, symlinks,
///      mixed slashes — the only authoritative resolver).
///   2. Manual `..` / `.` collapse on the component list when the file
///      doesn't exist (e.g. allowlist entries for uninstalled games loaded
///      from JSON — still want lookups to match).
///   3. Final `to_lowercase` + `/`→`\` so Windows path-comparison rules apply.
fn normalize(path: &Path) -> String {
    let resolved = canonicalize_or_collapse(path);
    resolved.to_string_lossy().to_lowercase().replace('/', "\\")
}

fn canonicalize_or_collapse(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return strip_unc_prefix(&p);
    }
    collapse_dots(path)
}

/// `std::fs::canonicalize` returns a `\\?\C:\…` UNC path on Windows; strip
/// the prefix so allowlist entries stay human-readable and match paths the
/// rest of the code passes around.
fn strip_unc_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let trimmed = s.strip_prefix(r"\\?\").unwrap_or(&s);
    PathBuf::from(trimmed)
}

/// Pure-syntactic `..` / `.` resolver — used as a fallback when the path
/// doesn't exist on disk (canonicalize would error). Doesn't follow symlinks
/// or convert to absolute; it just folds the components.
fn collapse_dots(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                // Only pop a real directory; don't underflow past a prefix
                // (drive letter / UNC root) or a RootDir.
                let pop = matches!(out.last(), Some(Component::Normal(_)));
                if pop {
                    out.pop();
                } else {
                    out.push(c);
                }
            }
            Component::CurDir => {}
            _ => out.push(c),
        }
    }
    let mut pb = PathBuf::new();
    for c in out {
        pb.push(c.as_os_str());
    }
    pb
}

// ---------- launcher scanning ----------

/// Scan known launcher install directories. Currently: Steam.
/// Returns a deduplicated list of game executable paths.
pub fn scan_launchers() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    out.extend(scan_steam());
    out
}

#[cfg(windows)]
fn scan_steam() -> Vec<PathBuf> {
    let mut games = Vec::new();
    for root in steam_library_roots() {
        let common = root.join("steamapps").join("common");
        let Ok(entries) = std::fs::read_dir(&common) else {
            continue;
        };
        for entry in entries.flatten() {
            let game_dir = entry.path();
            if !game_dir.is_dir() {
                continue;
            }
            // Top-level .exe files only — Steam games' main exe is almost
            // always at the install root. Subdir-only exes (rare) are missed
            // by design; user can add them manually.
            let Ok(files) = std::fs::read_dir(&game_dir) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                if p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false)
                {
                    games.push(p);
                }
            }
        }
    }
    games
}

/// Every Steam library root on this machine. Default install dir plus any
/// extra libraries declared in `<SteamPath>\steamapps\libraryfolders.vdf`
/// (which is how users with multiple drives organize Steam installs).
#[cfg(windows)]
fn steam_library_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Some(install) = steam_install_path() else {
        return roots;
    };
    roots.push(install.clone());

    // Parse libraryfolders.vdf with a minimal line scanner. The file is
    // Valve's KeyValues format; we only need the `"path"  "..."` entries:
    //
    //   "libraryfolders" {
    //     "0" { "path"  "C:\\Program Files (x86)\\Steam"  ... }
    //     "1" { "path"  "D:\\SteamLibrary"  ... }
    //   }
    //
    // Backslashes appear as `\\` in VDF, so we unescape after extraction.
    let vdf = install.join("steamapps").join("libraryfolders.vdf");
    let Ok(content) = std::fs::read_to_string(&vdf) else {
        return roots;
    };
    for line in content.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("\"path\"") else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(after_open) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(end) = after_open.find('"') else {
            continue;
        };
        let raw = &after_open[..end];
        let path = PathBuf::from(raw.replace("\\\\", "\\"));
        if !roots.iter().any(|r| r == &path) {
            roots.push(path);
        }
    }
    roots
}

#[cfg(windows)]
fn steam_install_path() -> Option<PathBuf> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_VALUE_TYPE,
    };

    let subkey: Vec<u16> = "Software\\Valve\\Steam\0".encode_utf16().collect();
    let value_name: Vec<u16> = "SteamPath\0".encode_utf16().collect();

    unsafe {
        let mut hkey: HKEY = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return None;
        }

        let mut buf = [0u16; 512];
        let mut buf_len = (buf.len() * 2) as u32;
        let mut value_type = REG_VALUE_TYPE::default();
        let result = RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut value_type),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut buf_len),
        );
        let _ = RegCloseKey(hkey);
        if result.is_err() {
            return None;
        }

        // buf_len is in bytes including the null terminator; convert to u16 count
        let chars = (buf_len as usize / 2).saturating_sub(1).min(buf.len());
        let null_idx = buf[..chars].iter().position(|&c| c == 0).unwrap_or(chars);
        let s = String::from_utf16_lossy(&buf[..null_idx]);
        if s.is_empty() {
            return None;
        }
        Some(PathBuf::from(s.replace('/', "\\")))
    }
}

// ---------- window-to-exe resolution ----------

/// Given a window handle, return the full path to the owning process's
/// executable.
#[cfg(windows)]
pub fn resolve_window_exe(hwnd_val: isize) -> Option<PathBuf> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    unsafe {
        let hwnd = HWND(hwnd_val as *mut _);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let r = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if r.is_err() {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buf[..size as usize],
        )))
    }
}

#[cfg(not(windows))]
pub fn resolve_window_exe(_hwnd_val: isize) -> Option<PathBuf> {
    None
}

// ---------- persistence ----------

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct AllowlistFile {
    /// User-added entries only. Launcher-scanned entries are recomputed every
    /// session so they stay accurate when the user installs/uninstalls games.
    manual: Vec<String>,
}

pub fn load_manual_entries(file: &Path) -> Vec<PathBuf> {
    let Ok(s) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let parsed: AllowlistFile = match serde_json::from_str(&s) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    parsed.manual.into_iter().map(PathBuf::from).collect()
}

pub fn save_manual_entries(file: &Path, entries: &[PathBuf]) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = AllowlistFile {
        manual: entries
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    };
    let s = serde_json::to_string_pretty(&payload).map_err(std::io::Error::other)?;
    std::fs::write(file, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_allowlist_is_empty() {
        let g = GameAllowlist::new();
        assert_eq!(g.len(), 0);
        assert!(g.entries().is_empty());
    }

    #[test]
    fn add_then_contains_matches_exact_path() {
        let mut g = GameAllowlist::new();
        let p = PathBuf::from(r"C:\Games\test\game.exe");
        g.add(&p);
        assert!(g.contains(&p));
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn contains_is_case_insensitive() {
        let mut g = GameAllowlist::new();
        g.add(&PathBuf::from(r"C:\Games\Foo\GAME.exe"));
        assert!(g.contains(&PathBuf::from(r"c:\games\foo\game.exe")));
        assert!(g.contains(&PathBuf::from(r"C:\GAMES\FOO\GAME.EXE")));
    }

    #[test]
    fn contains_normalizes_forward_slashes_to_back() {
        let mut g = GameAllowlist::new();
        g.add(&PathBuf::from("C:/Games/foo/bar.exe"));
        assert!(g.contains(&PathBuf::from(r"C:\Games\foo\bar.exe")));
    }

    #[test]
    fn duplicate_add_does_not_inflate_len() {
        let mut g = GameAllowlist::new();
        g.add(&PathBuf::from(r"C:\Games\a.exe"));
        g.add(&PathBuf::from(r"c:\GAMES\A.EXE")); // same after normalize
        assert_eq!(g.len(), 1);
    }

    #[test]
    fn remove_reports_whether_entry_existed() {
        let mut g = GameAllowlist::new();
        g.add(&PathBuf::from(r"C:\Games\a.exe"));
        assert!(g.remove(&PathBuf::from(r"C:\Games\a.exe")));
        assert!(!g.remove(&PathBuf::from(r"C:\Games\a.exe"))); // already gone
        assert_eq!(g.len(), 0);
    }

    #[test]
    fn extend_inserts_each_path_deduplicated() {
        let mut g = GameAllowlist::new();
        g.extend(vec![
            PathBuf::from(r"C:\a\one.exe"),
            PathBuf::from(r"C:\A\ONE.EXE"), // dup post-normalize
            PathBuf::from(r"C:\b\two.exe"),
        ]);
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn entries_are_sorted() {
        let mut g = GameAllowlist::new();
        g.add(&PathBuf::from(r"C:\b\two.exe"));
        g.add(&PathBuf::from(r"C:\a\one.exe"));
        let e = g.entries();
        assert_eq!(e.len(), 2);
        assert!(e[0] < e[1]);
    }

    #[test]
    fn normalize_is_idempotent() {
        let p = PathBuf::from(r"C:\Games\Test\Game.EXE");
        assert_eq!(normalize(&p), normalize(Path::new(&normalize(&p))));
    }

    #[test]
    fn normalize_collapses_dotdot_segments_for_nonexistent_paths() {
        // File doesn't exist → fallback collapser fires. The traversal in
        // the input must not survive into the stored key, otherwise
        // `add("...\\..\\evil.exe")` and `contains("evil.exe")` would miss.
        let a = normalize(Path::new(r"C:\Games\foo\..\bar.exe"));
        let b = normalize(Path::new(r"C:\Games\bar.exe"));
        assert_eq!(a, b);
    }

    #[test]
    fn normalize_collapses_curdir_segments() {
        let a = normalize(Path::new(r"C:\Games\.\bar.exe"));
        let b = normalize(Path::new(r"C:\Games\bar.exe"));
        assert_eq!(a, b);
    }

    #[test]
    fn allowlist_contains_after_traversal_normalization() {
        let mut g = GameAllowlist::new();
        g.add(Path::new(r"C:\Games\foo\..\bar.exe"));
        assert!(g.contains(Path::new(r"C:\Games\bar.exe")));
        assert_eq!(g.len(), 1);
    }
}
