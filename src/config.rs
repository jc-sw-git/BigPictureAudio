//! Settings in `%APPDATA%\BigPictureAudio\config.ini` plus a small log.

use std::collections::HashMap;
use std::path::PathBuf;

/// A device chosen by the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceRef {
    pub id: String,
    /// Friendly name at the time it was chosen.
    pub name: String,
    /// Whether no other active device had the same name when it was chosen.
    /// Only then may the name be used to find the device again if its ID
    /// changes (e.g. after a driver update).
    pub name_unique: bool,
}

/// What to do with the default output device when Big Picture is closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Leave {
    /// Keep whatever device is active.
    #[default]
    Stay,
    /// Switch back to the device that was the default before Big Picture.
    Previous,
    /// Switch to a specific device.
    Device(DeviceRef),
}

#[derive(Debug, PartialEq)]
pub struct Config {
    /// Device to switch to while in Big Picture mode.
    pub target: Option<DeviceRef>,
    pub leave: Leave,
    /// Skip the leave action if the user changed the default device manually
    /// during the session.
    pub skip_leave_if_manual: bool,
    /// UI language code (see `i18n`); `None` follows the Windows display language.
    pub language: Option<String>,
    /// `true` while a Big Picture session is in progress. Persisted together
    /// with `previous` and `expected` so the leave action still runs correctly
    /// after a crash or reboot.
    pub session: bool,
    /// Default device at the moment Big Picture started.
    pub previous: Option<String>,
    /// Default device as last set or seen by the app during the session; if the
    /// actual default differs, the user changed it manually.
    pub expected: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            target: None,
            leave: Leave::Stay,
            skip_leave_if_manual: true,
            language: None,
            session: false,
            previous: None,
            expected: None,
        }
    }
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
        let device = |key: &str| {
            get(key).map(|id| DeviceRef {
                id,
                name: get(&format!("{key}_name")).unwrap_or_default(),
                name_unique: get(&format!("{key}_unique")).as_deref() == Some("1"),
            })
        };

        let leave = match get("leave").as_deref() {
            Some("previous") => Leave::Previous,
            Some("device") => device("leave_device").map(Leave::Device).unwrap_or_default(),
            _ => Leave::Stay,
        };
        Config {
            target: device("target"),
            leave,
            skip_leave_if_manual: get("skip_leave_if_manual").as_deref() != Some("0"),
            language: get("language").filter(|v| v != "auto"),
            session: get("session").as_deref() == Some("1"),
            previous: get("previous"),
            expected: get("expected"),
        }
    }

    fn serialize(&self) -> String {
        fn device(out: &mut String, key: &str, device: Option<&DeviceRef>) {
            let empty = DeviceRef::default();
            let d = device.unwrap_or(&empty);
            let unique = if d.name_unique { "1" } else { "0" };
            out.push_str(&format!(
                "{key}={}\n{key}_name={}\n{key}_unique={unique}\n",
                d.id, d.name
            ));
        }
        let flag = |b: bool| if b { "1" } else { "0" };

        let mut out = String::new();
        device(&mut out, "target", self.target.as_ref());
        let (leave, leave_device) = match &self.leave {
            Leave::Stay => ("stay", None),
            Leave::Previous => ("previous", None),
            Leave::Device(d) => ("device", Some(d)),
        };
        out.push_str(&format!("leave={leave}\n"));
        device(&mut out, "leave_device", leave_device);
        out.push_str(&format!(
            "skip_leave_if_manual={}\nlanguage={}\nsession={}\nprevious={}\nexpected={}\n",
            flag(self.skip_leave_if_manual),
            self.language.as_deref().unwrap_or("auto"),
            flag(self.session),
            self.previous.as_deref().unwrap_or(""),
            self.expected.as_deref().unwrap_or(""),
        ));
        out
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Err(e) = std::fs::write(file(), self.serialize()) {
            log(&format!("Failed to save config: {e}"));
        }
    }
}

/// Appends a line to `log.txt`. Does nothing in tests, so they don't write
/// to the real log.
#[cfg(test)]
pub fn log(_message: &str) {}

#[cfg(not(test))]
pub fn log(message: &str) {
    use std::io::Write;
    use windows::Win32::System::SystemInformation::GetLocalTime;

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

    fn device(id: &str, unique: bool) -> DeviceRef {
        DeviceRef {
            id: id.into(),
            name: format!("Name {id}"),
            name_unique: unique,
        }
    }

    #[test]
    fn round_trips() {
        let config = Config {
            target: Some(device("{a}", true)),
            leave: Leave::Device(device("{b}", false)),
            skip_leave_if_manual: false,
            language: Some("de".into()),
            session: true,
            previous: Some("{c}".into()),
            expected: Some("{d}".into()),
        };
        assert_eq!(Config::parse(&config.serialize()), config);
        let config = Config {
            leave: Leave::Previous,
            ..Default::default()
        };
        assert_eq!(Config::parse(&config.serialize()), config);
    }

    #[test]
    fn reads_configs_from_older_versions() {
        let config = Config::parse("target={a}\nrestore=\n");
        assert_eq!(config.leave, Leave::Stay);
        assert!(config.skip_leave_if_manual);
        // Name is filled in at startup.
        assert_eq!(
            config.target,
            Some(DeviceRef {
                id: "{a}".into(),
                ..Default::default()
            })
        );
    }
}
