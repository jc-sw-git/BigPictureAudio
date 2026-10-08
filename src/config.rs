//! Settings in `%APPDATA%\BigPictureAudio\config.ini` plus a small log.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use windows::Win32::System::SystemInformation::GetLocalTime;

/// What to do with the default output device when Big Picture is closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Leave {
    /// Keep whatever device is active.
    #[default]
    Stay,
    /// Switch back to the device that was the default before Big Picture.
    Previous,
    /// Switch to a specific device.
    Device(String),
}

#[derive(Debug, Default, PartialEq)]
pub struct Config {
    /// Device to switch to while in Big Picture mode.
    pub target: Option<String>,
    pub leave: Leave,
    /// UI language code (see `i18n`); `None` follows the Windows display language.
    pub language: Option<String>,
    /// `true` while a Big Picture session is in progress. Persisted together
    /// with `previous` so the leave action still runs after a crash or reboot.
    pub session: bool,
    /// Default device at the moment Big Picture started.
    pub previous: Option<String>,
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("BigPictureAudio")
}

fn file() -> PathBuf {
    dir().join("config.ini")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(file())
            .map(|c| Self::parse(&c))
            .unwrap_or_default()
    }

    fn parse(content: &str) -> Self {
        let values: HashMap<&str, String> = content
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.trim(), value.trim().to_string()))
            .filter(|(_, value)| !value.is_empty())
            .collect();
        let get = |key: &str| values.get(key).cloned();

        let leave = match get("leave").as_deref() {
            Some("previous") => Leave::Previous,
            Some("device") => get("leave_device").map(Leave::Device).unwrap_or_default(),
            _ => Leave::Stay,
        };
        Config {
            target: get("target"),
            leave,
            language: get("language").filter(|v| v != "auto"),
            session: get("session").as_deref() == Some("1"),
            previous: get("previous"),
        }
    }

    fn serialize(&self) -> String {
        let (leave, leave_device) = match &self.leave {
            Leave::Stay => ("stay", ""),
            Leave::Previous => ("previous", ""),
            Leave::Device(id) => ("device", id.as_str()),
        };
        format!(
            "target={}\nleave={leave}\nleave_device={leave_device}\nlanguage={}\nsession={}\nprevious={}\n",
            self.target.as_deref().unwrap_or(""),
            self.language.as_deref().unwrap_or("auto"),
            if self.session { "1" } else { "0" },
            self.previous.as_deref().unwrap_or(""),
        )
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Err(e) = std::fs::write(file(), self.serialize()) {
            log(&format!("Failed to save config: {e}"));
        }
    }
}

pub fn log(message: &str) {
    let t = unsafe { GetLocalTime() };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}  {message}\n",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    );
    let _ = std::fs::create_dir_all(dir());
    let path = dir().join("log.txt");
    // Keep the log small: start over after 1 MB.
    let truncate = std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!truncate)
        .truncate(truncate)
        .open(path);
    if let Ok(mut file) = file {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let config = Config {
            target: Some("{a}".into()),
            leave: Leave::Device("{b}".into()),
            language: Some("de".into()),
            session: true,
            previous: Some("{c}".into()),
        };
        assert_eq!(Config::parse(&config.serialize()), config);
        let config = Config {
            leave: Leave::Previous,
            ..Default::default()
        };
        assert_eq!(Config::parse(&config.serialize()), config);
    }

    #[test]
    fn defaults_to_not_switching_back() {
        // Configs from older versions have no `leave` key.
        let config = Config::parse("target={a}\nrestore=\n");
        assert_eq!(config.leave, Leave::Stay);
        assert_eq!(config.target.as_deref(), Some("{a}"));
    }
}
