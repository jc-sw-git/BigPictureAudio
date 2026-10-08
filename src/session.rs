//! What happens to the default output device when Big Picture starts and
//! ends. Kept free of UI and talks to the audio system through [`Audio`], so
//! the logic can be tested without touching real devices.

use crate::audio::{self, Device};
use crate::config::{self, Config, DeviceRef, Leave};

/// Number of consecutive ticks Big Picture must be gone before it counts as
/// closed – absorbs short window transitions (e.g. while a game launches).
const EXIT_DEBOUNCE_TICKS: u32 = 2;
/// How long to wait for the device chosen for leaving Big Picture to show up
/// (e.g. a Bluetooth headset that reconnects) before giving up.
const LEAVE_RETRY_TICKS: u32 = 30;

pub trait Audio {
    fn outputs(&self) -> Vec<Device>;
    fn default_output(&self) -> Option<String>;
    fn set_default_output(&mut self, id: &str) -> bool;
    fn is_available(&self, id: &str) -> bool;
    fn is_gone(&self, id: &str) -> bool;
    fn name_of(&self, id: &str) -> Option<String>;

    /// Device name for log messages.
    fn name(&self, id: &str) -> String {
        self.name_of(id).unwrap_or_else(|| id.to_string())
    }
}

/// The real Windows audio system.
pub struct System;

impl Audio for System {
    fn outputs(&self) -> Vec<Device> {
        audio::outputs().unwrap_or_default()
    }
    fn default_output(&self) -> Option<String> {
        audio::default_output()
    }
    fn set_default_output(&mut self, id: &str) -> bool {
        match audio::set_default_output(id) {
            Ok(()) => true,
            Err(e) => {
                config::log(&format!("Switching to {} failed: {e}", self.name(id)));
                false
            }
        }
    }
    fn is_available(&self, id: &str) -> bool {
        audio::is_available(id)
    }
    fn is_gone(&self, id: &str) -> bool {
        audio::is_gone(id)
    }
    fn name_of(&self, id: &str) -> Option<String> {
        audio::name_of(id)
    }
}

/// Returns the ID to switch to if `device` is available. If its ID no longer
/// exists (e.g. after a driver update) and its name was unique when it was
/// chosen, the single active device with that name takes its place.
pub fn resolve(device: &mut DeviceRef, audio: &impl Audio) -> Option<String> {
    if audio.is_available(&device.id) {
        return Some(device.id.clone());
    }
    if !device.name_unique || device.name.is_empty() || !audio.is_gone(&device.id) {
        return None;
    }
    let outputs = audio.outputs();
    let mut matches = outputs.iter().filter(|d| d.name == device.name);
    let (Some(found), None) = (matches.next(), matches.next()) else {
        return None;
    };
    config::log(&format!(
        "Device ID changed, found {} again ({} -> {})",
        device.name, device.id, found.id
    ));
    device.id = found.id.clone();
    Some(device.id.clone())
}

/// Creates the reference to a device picked from the menu.
pub fn device_ref(device: &Device, all: &[Device]) -> DeviceRef {
    DeviceRef {
        id: device.id.clone(),
        name: device.name.clone(),
        name_unique: all.iter().filter(|d| d.name == device.name).count() == 1,
    }
}

/// Fills in name information for devices from configs of older versions.
/// Returns `true` if something changed.
pub fn complete(device: &mut DeviceRef, audio: &impl Audio) -> bool {
    if !device.name.is_empty() {
        return false;
    }
    device.name = audio.name_of(&device.id).unwrap_or_default();
    let same_name = audio.outputs().iter().filter(|d| d.name == device.name).count();
    device.name_unique = !device.name.is_empty() && same_name <= 1;
    true
}

/// Pending switch after Big Picture closed, retried while the device isn't
/// available yet.
enum LeaveTarget {
    /// The device that was the default before Big Picture.
    Previous(String),
    /// The device configured in `Leave::Device`.
    Configured,
}

#[derive(Default)]
pub struct Session {
    pub active: bool,
    /// Big Picture is running but the target device isn't available yet.
    pub waiting_for_target: bool,
    /// The user changed the default device during this session; nothing is
    /// switched anymore until the session ends.
    pub user_override: bool,
    /// The config changed and should be saved.
    pub config_changed: bool,
    inactive_ticks: u32,
    leave_target: Option<LeaveTarget>,
    leave_retries_left: u32,
}

impl Session {
    /// Called once per second with whether the Big Picture window is there.
    pub fn tick(&mut self, detected: bool, config: &mut Config, audio: &mut impl Audio) {
        if detected {
            self.inactive_ticks = 0;
            if !self.active {
                self.active = true;
                config::log("Big Picture started");
                self.start(config, audio);
            } else {
                self.check_manual_change(config, audio);
                if self.waiting_for_target && !self.user_override {
                    self.try_switch_to_target(config, audio);
                }
            }
        } else if self.active {
            self.inactive_ticks += 1;
            if self.inactive_ticks >= EXIT_DEBOUNCE_TICKS {
                self.active = false;
                config::log("Big Picture closed");
                self.end(config, audio);
            }
        } else if self.leave_target.is_some() {
            self.try_leave(config, audio);
        }
    }

    /// Called at startup: picks up a session that was running when the app
    /// last exited, or finishes one that ended while the app wasn't running.
    pub fn resume(&mut self, detected: bool, config: &mut Config, audio: &mut impl Audio) {
        if detected {
            config::log("Big Picture already running");
            self.active = true;
            self.start(config, audio);
        } else if config.session {
            config::log("Big Picture session from the last run did not end cleanly");
            self.end(config, audio);
        }
    }

    fn start(&mut self, config: &mut Config, audio: &mut impl Audio) {
        self.leave_target = None;
        self.waiting_for_target = false;
        if config.session {
            // The session was already running when the app (re)started. If
            // the default device isn't what the app last set or saw, the user
            // changed it in the meantime.
            self.user_override = default_changed(config, audio);
        } else {
            // Every start of Big Picture is a new session.
            self.user_override = false;
            config.session = true;
            config.previous = audio.default_output();
            config.expected = config.previous.clone();
            self.config_changed = true;
        }
        if self.user_override {
            config::log("Default device was changed manually – not switching");
            return;
        }
        self.try_switch_to_target(config, audio);
    }

    /// Detects the user switching devices during the session.
    fn check_manual_change(&mut self, config: &mut Config, audio: &impl Audio) {
        if self.user_override || !default_changed(config, audio) {
            return;
        }
        // Reaching the target device (e.g. Windows picked it by itself when the
        // TV was turned on) isn't a manual override.
        let current = audio.default_output();
        if current.is_some() && current.as_deref() == config.target.as_ref().map(|d| d.id.as_str()) {
            config.expected = current;
            self.config_changed = true;
            self.waiting_for_target = false;
            return;
        }
        self.user_override = true;
        self.waiting_for_target = false;
        let current = current.map(|id| audio.name(&id)).unwrap_or_default();
        config::log(&format!(
            "Default device changed manually to {current} – pausing until Big Picture closes"
        ));
    }

    fn try_switch_to_target(&mut self, config: &mut Config, audio: &mut impl Audio) {
        let Some(device) = config.target.as_mut() else {
            self.waiting_for_target = false;
            return;
        };
        let before = device.id.clone();
        let resolved = resolve(device, audio);
        self.config_changed |= device.id != before;
        let Some(id) = resolved else {
            if !self.waiting_for_target {
                let target = audio.name(&device.id);
                config::log(&format!("Target device not available, waiting for it: {target}"));
            }
            self.waiting_for_target = true;
            return;
        };
        self.waiting_for_target = false;
        if switch_to(&id, audio) {
            config.expected = Some(id);
            self.config_changed = true;
        }
    }

    fn end(&mut self, config: &mut Config, audio: &mut impl Audio) {
        self.waiting_for_target = false;
        let manual = self.user_override || default_changed(config, audio);
        self.user_override = false;
        let previous = config.previous.take();
        config.expected = None;
        config.session = false;
        self.config_changed = true;

        if manual && config.skip_leave_if_manual {
            config::log("Default device was changed manually – keeping it");
            self.leave_target = None;
            return;
        }
        self.leave_target = match &config.leave {
            Leave::Stay => None,
            Leave::Previous => previous.map(LeaveTarget::Previous),
            Leave::Device(_) => Some(LeaveTarget::Configured),
        };
        if self.leave_target.is_none() {
            config::log("Keeping the current device");
        }
        self.leave_retries_left = LEAVE_RETRY_TICKS;
        self.try_leave(config, audio);
    }

    fn resolve_leave_target(&mut self, config: &mut Config, audio: &impl Audio) -> Option<String> {
        match self.leave_target.as_ref()? {
            LeaveTarget::Previous(id) => audio.is_available(id).then(|| id.clone()),
            LeaveTarget::Configured => {
                let Leave::Device(device) = &mut config.leave else {
                    return None;
                };
                let before = device.id.clone();
                let id = resolve(device, audio);
                self.config_changed |= device.id != before;
                id
            }
        }
    }

    fn try_leave(&mut self, config: &mut Config, audio: &mut impl Audio) {
        if let Some(id) = self.resolve_leave_target(config, audio) {
            self.leave_target = None;
            switch_to(&id, audio);
            return;
        }
        if self.leave_retries_left == LEAVE_RETRY_TICKS {
            config::log(&format!("Device not available, waiting up to {LEAVE_RETRY_TICKS} s"));
        }
        if self.leave_retries_left == 0 {
            config::log("Device did not become available – not switching");
            self.leave_target = None;
            return;
        }
        self.leave_retries_left -= 1;
    }

    /// The user picked a new target device. If Big Picture is running, it's
    /// applied right away – choosing a device explicitly ends a manual override.
    pub fn set_target(&mut self, device: Option<DeviceRef>, config: &mut Config, audio: &mut impl Audio) {
        config.target = device;
        self.config_changed = true;
        if self.active {
            self.user_override = false;
            self.waiting_for_target = false;
            self.try_switch_to_target(config, audio);
        }
    }

    /// The app is exiting: during Big Picture this behaves like leaving it
    /// (single attempt, no waiting for devices).
    pub fn shutdown(&mut self, config: &mut Config, audio: &mut impl Audio) {
        if self.active {
            self.active = false;
            self.end(config, audio);
        }
    }
}

/// `true` if the default device isn't the one the app last set or saw.
fn default_changed(config: &Config, audio: &impl Audio) -> bool {
    config.expected.is_some() && audio.default_output() != config.expected
}

/// Makes `id` the default output device unless it already is. Returns `true`
/// if it is the default afterwards.
fn switch_to(id: &str, audio: &mut impl Audio) -> bool {
    if audio.default_output().as_deref() == Some(id) {
        config::log(&format!("Already the default device: {}", audio.name(id)));
        return true;
    }
    let switched = audio.set_default_output(id);
    if switched {
        config::log(&format!("Switched to: {}", audio.name(id)));
    }
    switched
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulated audio system: a list of devices, which of them are active,
    /// and the current default.
    struct Fake {
        devices: Vec<(String, String, bool)>,
        default: Option<String>,
        switches: Vec<String>,
    }

    impl Fake {
        fn new(devices: &[(&str, &str, bool)], default: &str) -> Self {
            Fake {
                devices: devices
                    .iter()
                    .map(|(i, n, a)| (i.to_string(), n.to_string(), *a))
                    .collect(),
                default: Some(default.into()),
                switches: Vec::new(),
            }
        }
        fn set_active(&mut self, id: &str, active: bool) {
            self.devices.iter_mut().find(|d| d.0 == id).unwrap().2 = active;
        }
        /// The user switching devices in Windows.
        fn user_switches_to(&mut self, id: &str) {
            self.default = Some(id.into());
        }
    }

    impl Audio for Fake {
        fn outputs(&self) -> Vec<Device> {
            self.devices
                .iter()
                .filter(|d| d.2)
                .map(|(id, name, _)| Device {
                    id: id.clone(),
                    name: name.clone(),
                    label: name.clone(),
                })
                .collect()
        }
        fn default_output(&self) -> Option<String> {
            self.default.clone()
        }
        fn set_default_output(&mut self, id: &str) -> bool {
            self.default = Some(id.into());
            self.switches.push(id.into());
            true
        }
        fn is_available(&self, id: &str) -> bool {
            self.devices.iter().any(|d| d.0 == id && d.2)
        }
        fn is_gone(&self, id: &str) -> bool {
            !self.devices.iter().any(|d| d.0 == id)
        }
        fn name_of(&self, id: &str) -> Option<String> {
            self.devices.iter().find(|d| d.0 == id).map(|d| d.1.clone())
        }
    }

    const DEVICES: &[(&str, &str, bool)] = &[
        ("speakers", "Speakers", true),
        ("tv", "LG TV", true),
        ("headset", "Headset", true),
    ];

    fn config(leave: Leave) -> Config {
        Config {
            target: Some(DeviceRef {
                id: "tv".into(),
                name: "LG TV".into(),
                name_unique: true,
            }),
            leave,
            ..Default::default()
        }
    }

    /// Runs `n` ticks with Big Picture visible or not.
    fn ticks(n: u32, detected: bool, s: &mut Session, c: &mut Config, a: &mut Fake) {
        for _ in 0..n {
            s.tick(detected, c, a);
        }
    }

    #[test]
    fn switches_on_start_and_back_on_leave() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("tv"));
        ticks(2, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("speakers"));
        assert!(!c.session);
    }

    #[test]
    fn stays_by_default() {
        let (mut s, mut c, mut a) = (Session::default(), config(Leave::Stay), Fake::new(DEVICES, "speakers"));
        ticks(1, true, &mut s, &mut c, &mut a);
        ticks(2, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("tv"));
    }

    #[test]
    fn short_gaps_dont_end_the_session() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        ticks(1, false, &mut s, &mut c, &mut a);
        ticks(1, true, &mut s, &mut c, &mut a);
        assert!(s.active);
        assert_eq!(a.switches, ["tv"]);
    }

    #[test]
    fn waits_for_target_to_become_available() {
        let (mut s, mut c, mut a) = (Session::default(), config(Leave::Stay), Fake::new(DEVICES, "speakers"));
        a.set_active("tv", false);
        ticks(3, true, &mut s, &mut c, &mut a);
        assert!(s.waiting_for_target);
        assert_eq!(a.default.as_deref(), Some("speakers"));
        a.set_active("tv", true);
        ticks(1, true, &mut s, &mut c, &mut a);
        assert!(!s.waiting_for_target);
        assert_eq!(a.default.as_deref(), Some("tv"));
    }

    #[test]
    fn manual_change_stops_waiting_for_target() {
        let (mut s, mut c, mut a) = (Session::default(), config(Leave::Stay), Fake::new(DEVICES, "speakers"));
        a.set_active("tv", false);
        ticks(2, true, &mut s, &mut c, &mut a);
        a.user_switches_to("headset");
        ticks(1, true, &mut s, &mut c, &mut a);
        a.set_active("tv", true);
        ticks(5, true, &mut s, &mut c, &mut a);
        assert!(s.user_override);
        assert_eq!(a.default.as_deref(), Some("headset"));
    }

    #[test]
    fn windows_picking_the_target_is_not_a_manual_change() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        a.set_active("tv", false);
        ticks(2, true, &mut s, &mut c, &mut a);
        a.set_active("tv", true);
        a.user_switches_to("tv");
        ticks(1, true, &mut s, &mut c, &mut a);
        assert!(!s.user_override);
        ticks(2, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("speakers"));
    }

    #[test]
    fn manual_change_skips_leave_action_by_default() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        a.user_switches_to("headset");
        ticks(3, true, &mut s, &mut c, &mut a);
        ticks(2, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("headset"));
    }

    #[test]
    fn manual_change_with_leave_action_enabled() {
        let mut c = Config {
            skip_leave_if_manual: false,
            ..config(Leave::Previous)
        };
        let (mut s, mut a) = (Session::default(), Fake::new(DEVICES, "speakers"));
        ticks(1, true, &mut s, &mut c, &mut a);
        a.user_switches_to("headset");
        ticks(3, true, &mut s, &mut c, &mut a);
        ticks(2, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("speakers"));
    }

    #[test]
    fn every_start_of_big_picture_is_a_new_session() {
        let (mut s, mut c, mut a) = (Session::default(), config(Leave::Stay), Fake::new(DEVICES, "speakers"));
        ticks(1, true, &mut s, &mut c, &mut a);
        a.user_switches_to("headset");
        ticks(1, true, &mut s, &mut c, &mut a);
        assert!(s.user_override);
        // Leaving and re-entering shortly after switches again.
        ticks(2, false, &mut s, &mut c, &mut a);
        ticks(1, true, &mut s, &mut c, &mut a);
        assert!(!s.user_override);
        assert_eq!(a.default.as_deref(), Some("tv"));
        assert_eq!(c.previous.as_deref(), Some("headset"));
    }

    #[test]
    fn app_restart_during_session_keeps_manual_change() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        a.user_switches_to("headset");
        // App restarts (config persisted, session state lost).
        let mut s = Session::default();
        s.resume(true, &mut c, &mut a);
        assert!(s.user_override);
        assert_eq!(a.default.as_deref(), Some("headset"));
    }

    #[test]
    fn crash_during_session_runs_leave_action_on_next_start() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        let mut s = Session::default();
        s.resume(false, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("speakers"));
        assert!(!c.session);
    }

    #[test]
    fn crash_recovery_respects_manual_change() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        // Days later, after a crash, the user had picked the headset.
        a.user_switches_to("headset");
        let mut s = Session::default();
        s.resume(false, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("headset"));
    }

    #[test]
    fn leave_device_is_waited_for_a_limited_time() {
        let (mut s, mut c, mut a) = (
            Session::default(),
            config(Leave::Previous),
            Fake::new(DEVICES, "speakers"),
        );
        ticks(1, true, &mut s, &mut c, &mut a);
        a.set_active("speakers", false);
        ticks(2, false, &mut s, &mut c, &mut a);
        ticks(5, false, &mut s, &mut c, &mut a);
        a.set_active("speakers", true);
        ticks(1, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("speakers"));

        ticks(1, true, &mut s, &mut c, &mut a);
        a.set_active("speakers", false);
        ticks(2 + LEAVE_RETRY_TICKS + 5, false, &mut s, &mut c, &mut a);
        a.set_active("speakers", true);
        ticks(5, false, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("tv"));
    }

    #[test]
    fn finds_device_again_by_unique_name_after_id_change() {
        let mut a = Fake::new(&[("speakers", "Speakers", true), ("tv-new", "LG TV", true)], "speakers");
        let (mut s, mut c) = (Session::default(), config(Leave::Stay));
        ticks(1, true, &mut s, &mut c, &mut a);
        assert_eq!(a.default.as_deref(), Some("tv-new"));
        assert_eq!(c.target.unwrap().id, "tv-new");
    }

    #[test]
    fn does_not_guess_between_devices_sharing_a_name() {
        let devices = &[("speakers", "Speakers", true), ("m2", "Monitor", true)];
        let mut a = Fake::new(devices, "speakers");
        let mut c = Config {
            target: Some(DeviceRef {
                id: "m1".into(),
                name: "Monitor".into(),
                name_unique: false,
            }),
            ..Default::default()
        };
        let mut s = Session::default();
        ticks(2, true, &mut s, &mut c, &mut a);
        assert!(s.waiting_for_target);
        assert_eq!(a.default.as_deref(), Some("speakers"));
    }

    #[test]
    fn unplugged_device_is_waited_for_not_replaced() {
        // "tv" still exists but is unplugged; another device with the same name
        // must not take its place.
        let devices = &[
            ("speakers", "Speakers", true),
            ("tv", "LG TV", false),
            ("tv2", "LG TV", true),
        ];
        let (mut s, mut c, mut a) = (Session::default(), config(Leave::Stay), Fake::new(devices, "speakers"));
        ticks(1, true, &mut s, &mut c, &mut a);
        assert!(s.waiting_for_target);
        assert_eq!(a.default.as_deref(), Some("speakers"));
    }

    #[test]
    fn choosing_a_target_during_session_ends_override() {
        let (mut s, mut c, mut a) = (Session::default(), config(Leave::Stay), Fake::new(DEVICES, "speakers"));
        ticks(1, true, &mut s, &mut c, &mut a);
        a.user_switches_to("headset");
        ticks(1, true, &mut s, &mut c, &mut a);
        let tv = c.target.clone();
        s.set_target(tv, &mut c, &mut a);
        assert!(!s.user_override);
        assert_eq!(a.default.as_deref(), Some("tv"));
    }
}
