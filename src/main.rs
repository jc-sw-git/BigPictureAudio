#![windows_subsystem = "windows"]

//! Tray app: switches to a chosen audio output device when Steam enters Big
//! Picture mode and, if configured, to another device when it leaves.

mod audio;
mod config;
mod i18n;
mod session;
mod steam;

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use config::{DeviceRef, Leave};
use i18n::Texts;
use session::{Session, System};

const POLL_MS: u32 = 1000;
/// How often to check whether Steam's localization files changed.
const TITLES_CHECK_TICKS: u32 = 60;
/// How long to keep trying to add the tray icon (Explorer may not be ready
/// yet right after login).
const ICON_RETRY_TICKS: u32 = 60;

const WM_TRAY: u32 = WM_APP + 1;
const TIMER_ID: usize = 1;

const ID_TARGET_NONE: usize = 900;
const ID_LEAVE_STAY: usize = 901;
const ID_LEAVE_PREVIOUS: usize = 902;
const ID_LEAVE_SKIP_IF_MANUAL: usize = 903;
const ID_LANGUAGE_AUTO: usize = 904;
const ID_AUTOSTART: usize = 905;
const ID_OPEN_LOG: usize = 906;
const ID_EXIT: usize = 907;
const ID_TARGET_BASE: usize = 1000;
const ID_LEAVE_BASE: usize = 2000;
const ID_LANGUAGE_BASE: usize = 3000;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("BigPictureAudio");

struct App {
    hwnd: HWND,
    config: config::Config,
    texts: &'static Texts,
    titles: HashSet<String>,
    titles_stamp: steam::Stamp,
    tick: u32,
    session: Session,
    menu_devices: Vec<audio::Device>,
    taskbar_created: u32,
    icon: HICON,
    icon_added: bool,
    icon_retries_left: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Calls `f` with the app state. If the state is already borrowed (e.g. the
/// popup menu runs its own message loop and WM_TIMER arrives meanwhile), the
/// call is skipped instead of panicking.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| app.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Device name for log messages.
fn name(id: &str) -> String {
    audio::name_of(id).unwrap_or_else(|| id.to_string())
}

/// Device name for the UI (numbered if several devices share it), marked if
/// the device isn't connected.
fn label(id: &str, texts: &Texts) -> String {
    match audio::outputs()
        .ok()
        .and_then(|list| list.into_iter().find(|d| d.id == id))
    {
        Some(device) => device.label,
        None => format!("{} ({})", name(id), texts.not_connected),
    }
}

/// Loads the embedded app icon (resource ID 1, see build.rs) at the small-icon
/// size the system uses for the tray.
fn load_tray_icon(instance: HINSTANCE) -> HICON {
    unsafe {
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        // MAKEINTRESOURCE(1)
        let id = PCWSTR(std::ptr::without_provenance(1));
        LoadImageW(Some(instance), id, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR)
            .map(|handle| HICON(handle.0))
            .or_else(|_| LoadIconW(None, IDI_APPLICATION))
            .unwrap_or_default()
    }
}

impl App {
    fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.tick.is_multiple_of(TITLES_CHECK_TICKS) {
            self.reload_titles_if_changed();
        }
        if !self.icon_added && self.icon_retries_left > 0 {
            self.icon_retries_left -= 1;
            self.add_icon();
        }
        let s = &self.session;
        let before = (s.active, s.waiting_for_target, s.user_override);
        let detected = steam::big_picture_active(&self.titles);
        self.session.tick(detected, &mut self.config, &mut System);
        self.save_if_changed();
        let s = &self.session;
        if before != (s.active, s.waiting_for_target, s.user_override) {
            self.update_tooltip();
        }
    }

    fn save_if_changed(&mut self) {
        if std::mem::take(&mut self.session.config_changed) {
            self.config.save();
        }
    }

    fn reload_titles_if_changed(&mut self) {
        let stamp = steam::localization_stamp();
        if stamp != self.titles_stamp {
            self.titles = steam::load_titles();
            self.titles_stamp = stamp;
            config::log(&format!(
                "Steam localization changed – reloaded {} Big Picture window titles",
                self.titles.len()
            ));
        }
    }

    fn notify_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icon,
            ..Default::default()
        }
    }

    fn tooltip(&self) -> String {
        let t = self.texts;
        let state = if self.session.active {
            t.state_active
        } else {
            t.state_inactive
        };
        let mut tip = format!("Big Picture Audio\n{state}\n");
        if self.session.user_override {
            tip += t.manual_override;
            tip += "\n";
        } else if self.session.waiting_for_target {
            tip += t.waiting_for_device;
            tip += "\n";
        }
        let target = match &self.config.target {
            Some(device) => label(&device.id, t),
            None => t.no_device.into(),
        };
        tip + &format!("{}: {target}", t.target)
    }

    fn add_icon(&mut self) {
        let mut nid = self.notify_data();
        copy_wide(&mut nid.szTip, &self.tooltip());
        self.icon_added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid).as_bool() };
        if !self.icon_added && self.icon_retries_left == 0 {
            config::log("Could not add the tray icon");
        }
    }

    fn update_tooltip(&self) {
        let mut nid = self.notify_data();
        copy_wide(&mut nid.szTip, &self.tooltip());
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn balloon(&self, text: &str) {
        let mut nid = self.notify_data();
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        copy_wide(&mut nid.szTip, &self.tooltip());
        copy_wide(&mut nid.szInfoTitle, "Big Picture Audio");
        copy_wide(&mut nid.szInfo, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    fn remove_icon(&self) {
        let nid = self.notify_data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }

    /// Appends the device list; a selected device that isn't connected is
    /// shown greyed out so the current choice stays visible.
    unsafe fn append_devices(&self, menu: HMENU, id_base: usize, selected: Option<&str>) {
        for (i, device) in self.menu_devices.iter().enumerate() {
            let checked = selected == Some(device.id.as_str());
            append(menu, MF_STRING | check(checked), id_base + i, &device.label);
        }
        if let Some(id) = selected.filter(|id| !self.menu_devices.iter().any(|d| d.id == *id)) {
            append(menu, MF_STRING | MF_GRAYED | MF_CHECKED, 0, &label(id, self.texts));
        }
    }

    fn show_menu(&mut self) {
        self.menu_devices = audio::outputs().unwrap_or_default();
        let t = self.texts;
        unsafe {
            let (Ok(menu), Ok(enter), Ok(leave), Ok(languages)) = (
                CreatePopupMenu(),
                CreatePopupMenu(),
                CreatePopupMenu(),
                CreatePopupMenu(),
            ) else {
                return;
            };

            append(
                menu,
                MF_STRING | MF_GRAYED,
                0,
                if self.session.active {
                    t.state_active
                } else {
                    t.state_inactive
                },
            );
            if self.session.user_override {
                append(menu, MF_STRING | MF_GRAYED, 0, t.manual_override);
            } else if self.session.waiting_for_target {
                append(menu, MF_STRING | MF_GRAYED, 0, t.waiting_for_device);
            }
            append(menu, MF_SEPARATOR, 0, "");

            let target = self.config.target.as_ref().map(|d| d.id.as_str());
            append(
                enter,
                MF_STRING | check(target.is_none()),
                ID_TARGET_NONE,
                t.dont_switch,
            );
            append(enter, MF_SEPARATOR, 0, "");
            self.append_devices(enter, ID_TARGET_BASE, target);
            append(menu, MF_POPUP, enter.0 as usize, t.enter_menu);

            let leave_device = match &self.config.leave {
                Leave::Device(device) => Some(device.id.as_str()),
                _ => None,
            };
            let stay = self.config.leave == Leave::Stay;
            append(leave, MF_STRING | check(stay), ID_LEAVE_STAY, t.dont_switch);
            // During Big Picture that's the device from before it started;
            // otherwise the current default, which it will be at the next start.
            let previous_id = if self.session.active {
                self.config.previous.clone()
            } else {
                audio::default_output()
            };
            let previous_text = match previous_id {
                Some(id) => t.labeled(t.previous_device, &label(&id, t)),
                None => t.previous_device.to_string(),
            };
            let previous = self.config.leave == Leave::Previous;
            append(leave, MF_STRING | check(previous), ID_LEAVE_PREVIOUS, &previous_text);
            append(leave, MF_SEPARATOR, 0, "");
            self.append_devices(leave, ID_LEAVE_BASE, leave_device);
            append(leave, MF_SEPARATOR, 0, "");
            // Irrelevant when nothing is switched on leaving anyway.
            let enabled = if stay { MF_GRAYED } else { MF_ENABLED };
            let skip = check(self.config.skip_leave_if_manual);
            append(
                leave,
                MF_STRING | skip | enabled,
                ID_LEAVE_SKIP_IF_MANUAL,
                t.skip_if_manual,
            );
            append(menu, MF_POPUP, leave.0 as usize, t.leave_menu);
            append(menu, MF_SEPARATOR, 0, "");

            let auto = self.config.language.is_none();
            append(languages, MF_STRING | check(auto), ID_LANGUAGE_AUTO, t.language_auto);
            append(languages, MF_SEPARATOR, 0, "");
            for (i, lang) in i18n::LANGUAGES.iter().enumerate() {
                let checked = self.config.language.as_deref() == Some(lang.code);
                append(
                    languages,
                    MF_STRING | check(checked),
                    ID_LANGUAGE_BASE + i,
                    lang.native_name,
                );
            }
            append(menu, MF_POPUP, languages.0 as usize, t.language);

            append(menu, MF_STRING | check(autostart_enabled()), ID_AUTOSTART, t.autostart);
            append(menu, MF_STRING, ID_OPEN_LOG, t.open_log);
            append(menu, MF_SEPARATOR, 0, "");
            append(menu, MF_STRING, ID_EXIT, t.exit);

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // Required so the menu closes when clicking elsewhere.
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                None,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            self.on_command(cmd.0 as usize);
        }
    }

    fn on_command(&mut self, cmd: usize) {
        let devices = &self.menu_devices;
        let device = |base: usize| {
            devices
                .get(cmd.wrapping_sub(base))
                .map(|d| session::device_ref(d, devices))
        };
        match cmd {
            ID_TARGET_NONE => self.set_target(None),
            ID_LEAVE_STAY => self.set_leave(Leave::Stay),
            ID_LEAVE_PREVIOUS => self.set_leave(Leave::Previous),
            ID_LEAVE_SKIP_IF_MANUAL => {
                self.config.skip_leave_if_manual = !self.config.skip_leave_if_manual;
                self.config.save();
            }
            ID_LANGUAGE_AUTO => self.set_language(None),
            ID_AUTOSTART => set_autostart(!autostart_enabled()),
            ID_OPEN_LOG => {
                let _ = std::fs::create_dir_all(config::dir());
                let _ = std::process::Command::new("explorer").arg(config::dir()).spawn();
            }
            ID_EXIT => unsafe {
                // Posted asynchronously so WM_DESTROY doesn't run inside this borrow.
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            ID_TARGET_BASE..ID_LEAVE_BASE => {
                if let Some(device) = device(ID_TARGET_BASE) {
                    self.set_target(Some(device));
                }
            }
            ID_LEAVE_BASE..ID_LANGUAGE_BASE => {
                if let Some(device) = device(ID_LEAVE_BASE) {
                    self.set_leave(Leave::Device(device));
                }
            }
            ID_LANGUAGE_BASE.. => {
                if let Some(lang) = i18n::LANGUAGES.get(cmd - ID_LANGUAGE_BASE) {
                    self.set_language(Some(lang.code));
                }
            }
            _ => {}
        }
    }

    fn set_target(&mut self, device: Option<DeviceRef>) {
        let shown = device.as_ref().map_or("none", |d| d.name.as_str());
        config::log(&format!("Target device: {shown}"));
        self.session.set_target(device, &mut self.config, &mut System);
        self.save_if_changed();
        self.update_tooltip();
    }

    fn set_leave(&mut self, leave: Leave) {
        config::log(&format!(
            "When leaving Big Picture: {}",
            match &leave {
                Leave::Stay => "don't switch",
                Leave::Previous => "previous device",
                Leave::Device(device) => &device.name,
            }
        ));
        self.config.leave = leave;
        self.config.save();
    }

    fn set_language(&mut self, code: Option<&str>) {
        self.config.language = code.map(str::to_string);
        self.config.save();
        self.texts = i18n::resolve(code);
        self.update_tooltip();
    }

    fn shutdown(&mut self) {
        self.session.shutdown(&mut self.config, &mut System);
        self.save_if_changed();
        self.remove_icon();
    }
}

fn check(checked: bool) -> MENU_ITEM_FLAGS {
    if checked {
        MF_CHECKED
    } else {
        MF_UNCHECKED
    }
}

unsafe fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    let _ = AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr()));
}

fn copy_wide(dst: &mut [u16], text: &str) {
    let wide: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
    dst[..wide.len()].copy_from_slice(&wide);
    dst[wide.len()] = 0;
}

fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn set_autostart(enable: bool) {
    unsafe {
        if enable {
            let Ok(exe) = std::env::current_exe() else { return };
            let command = format!("\"{}\"", exe.display());
            let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(wide.as_ptr() as *const c_void),
                (wide.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            with_app(App::on_tick);
            LRESULT(0)
        }
        WM_TRAY => {
            let event = (lparam.0 & 0xffff) as u32;
            if event == WM_RBUTTONUP || event == WM_LBUTTONUP {
                with_app(App::show_menu);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_app(App::shutdown);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_ENDSESSION if wparam.0 != 0 => {
            with_app(App::shutdown);
            LRESULT(0)
        }
        _ => {
            // Explorer was restarted → re-create the tray icon.
            if with_app(|app| app.taskbar_created == msg).unwrap_or(false) {
                with_app(App::add_icon);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

fn main() -> windows::core::Result<()> {
    unsafe {
        let _mutex = CreateMutexW(None, true, w!("Local\\BigPictureAudio.SingleInstance"))?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(());
        }
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;

        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("BigPictureAudioWindow"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class.lpszClassName,
            w!("Big Picture Audio"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let titles_stamp = steam::localization_stamp();
        let titles = steam::load_titles();
        config::log(&format!("Started – loaded {} Big Picture window titles", titles.len()));

        let mut config = config::Config::load();
        let mut completed = config.target.as_mut().is_some_and(|d| session::complete(d, &System));
        if let Leave::Device(device) = &mut config.leave {
            completed |= session::complete(device, &System);
        }
        if completed {
            config.save();
        }

        let detected = steam::big_picture_active(&titles);
        let mut app = App {
            hwnd,
            texts: i18n::resolve(config.language.as_deref()),
            config,
            titles,
            titles_stamp,
            tick: 0,
            session: Session::default(),
            menu_devices: Vec::new(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            icon: load_tray_icon(instance.into()),
            icon_added: false,
            icon_retries_left: ICON_RETRY_TICKS,
        };
        app.session.resume(detected, &mut app.config, &mut System);
        app.save_if_changed();

        app.add_icon();
        if app.config.target.is_none() {
            app.balloon(app.texts.first_run);
        }
        APP.with(|cell| *cell.borrow_mut() = Some(app));

        SetTimer(Some(hwnd), TIMER_ID, POLL_MS, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

#[cfg(test)]
mod system_tests {
    use super::*;

    /// Runs against the real system: `cargo test -- --ignored --nocapture`.
    /// Re-applies the current default device (no visible change).
    #[test]
    #[ignore]
    fn smoke() {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let titles = steam::load_titles();
        println!("Titles loaded: {}", titles.len());
        println!("Localization files: {}", steam::localization_stamp().len());
        println!("Big Picture active: {}", steam::big_picture_active(&titles));
        println!("UI language: {}", i18n::resolve(None).native_name);
        for d in audio::outputs().unwrap() {
            assert!(audio::is_available(&d.id));
            assert!(!audio::is_gone(&d.id));
            println!("Device: {} ({})", d.label, d.id);
        }
        let unknown = "{0.0.0.00000000}.{00000000-0000-0000-0000-000000000000}";
        assert!(!audio::is_available(unknown));
        assert!(audio::is_gone(unknown));
        let current = audio::default_output().expect("no default device");
        println!("Default: {}", name(&current));
        audio::set_default_output(&current).unwrap();
        assert_eq!(audio::default_output().as_deref(), Some(current.as_str()));
    }
}

#[cfg(test)]
mod diagnostics {
    use super::*;

    /// Logs default device, target state and Big Picture detection every 250 ms
    /// for `BPA_MONITOR_SECS` seconds. Read-only:
    /// `cargo test monitor -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn monitor() {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
        let secs: u64 = std::env::var("BPA_MONITOR_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);
        let titles = steam::load_titles();
        let target = config::Config::load().target.map(|d| d.id);
        let start = std::time::Instant::now();
        let mut last = String::new();
        while start.elapsed().as_secs() < secs {
            let default = audio::default_output().map(|id| name(&id)).unwrap_or_default();
            let target_state = target.as_deref().is_some_and(audio::is_available);
            let bpm = steam::big_picture_active(&titles);
            let line = format!("bpm={bpm} target_active={target_state} default={default}");
            if line != last {
                let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
                println!(
                    "{:02}:{:02}:{:02}.{:03}  {line}",
                    t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
                );
                last = line;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}
