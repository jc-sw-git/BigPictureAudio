//! Detection of Steam's Big Picture mode.
//!
//! The Big Picture window is an `SDL_app` window owned by `steamwebhelper.exe`
//! whose title is localized to the Steam client language (English: "Steam Big
//! Picture Mode", German: "Big-Picture-Modus", ...). Instead of hard-coding the
//! titles, they are read from Steam's own localization files
//! (`SP_WindowTitle_BigPicture`), so detection works in every language and
//! keeps working after Steam updates.

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;

use windows::core::{w, BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

const LOCALIZATION_KEY: &str = "\"SP_WindowTitle_BigPicture\":\"";

/// Fallback in case the localization files can't be found.
const FALLBACK_TITLES: &[&str] = &["Steam Big Picture Mode", "Big-Picture-Modus"];

pub fn steam_path() -> Option<PathBuf> {
    let mut buf = [0u16; 520];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Valve\\Steam"),
            w!("SteamPath"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

/// Collects the Big Picture window titles of all Steam languages.
pub fn load_titles() -> HashSet<String> {
    let mut titles: HashSet<String> = FALLBACK_TITLES.iter().map(|t| normalize(t)).collect();
    let Some(dir) = steam_path().map(|p| p.join("steamui").join("localization")) else {
        return titles;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return titles;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("steamui_") && name.ends_with("-json.js")) {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            if let Some(title) = extract_title(&content) {
                titles.insert(normalize(&title));
            }
        }
    }
    titles
}

fn extract_title(js: &str) -> Option<String> {
    let start = js.find(LOCALIZATION_KEY)? + LOCALIZATION_KEY.len();
    let mut out = String::new();
    let mut chars = js[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'u' => out.push(hex_char(&mut chars, 4)?),
                'x' => out.push(hex_char(&mut chars, 2)?),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

fn hex_char(chars: &mut std::str::Chars, digits: usize) -> Option<char> {
    let hex: String = chars.take(digits).collect();
    char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
}

/// Makes title comparison tolerant of non-breaking spaces and surrounding
/// whitespace (e.g. the Japanese title ends with a space).
fn normalize(title: &str) -> String {
    title.replace('\u{a0}', " ").trim().to_lowercase()
}

fn wide_to_string(buf: &[u16], len: i32) -> String {
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        result.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(|s| s.to_lowercase())
    }
}

struct Search<'a> {
    titles: &'a HashSet<String>,
    found: bool,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let search = &mut *(lparam.0 as *mut Search);
    if !IsWindowVisible(hwnd).as_bool() {
        return true.into();
    }
    let mut buf = [0u16; 64];
    let len = GetClassNameW(hwnd, &mut buf);
    let class = wide_to_string(&buf, len);
    if class != "SDL_app" {
        return true.into();
    }
    let mut buf = [0u16; 256];
    let len = GetWindowTextW(hwnd, &mut buf);
    let title = wide_to_string(&buf, len);
    if !search.titles.contains(&normalize(&title)) {
        return true.into();
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if matches!(process_name(pid).as_deref(), Some("steamwebhelper.exe" | "steam.exe")) {
        search.found = true;
        return false.into();
    }
    true.into()
}

/// `true` while a visible Big Picture window exists.
pub fn big_picture_active(titles: &HashSet<String>) -> bool {
    let mut search = Search { titles, found: false };
    unsafe {
        // EnumWindows reports an "error" when we stop early by returning `false`.
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_escaped_titles() {
        let js = r#"x={"SP_WindowTitle_BigPicture":"Steam\xA0: mode Big\xA0Picture","y":"z"}"#;
        assert_eq!(extract_title(js).unwrap(), "Steam\u{a0}: mode Big\u{a0}Picture");
        let js = r#"{"SP_WindowTitle_BigPicture":"Steam 大屏幕模式"}"#;
        assert_eq!(extract_title(js).unwrap(), "Steam 大屏幕模式");
    }

    #[test]
    fn normalizes_nbsp_and_whitespace() {
        assert_eq!(
            normalize("Steam\u{a0}: mode Big\u{a0}Picture "),
            "steam : mode big picture"
        );
    }
}
