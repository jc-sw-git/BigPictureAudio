//! Enumerate audio endpoints and set the default output device.
//!
//! Windows has no public API for setting the default device. All common tools
//! (EarTrumpet, SoundSwitch, AudioSwitcher, ...) use the undocumented COM
//! interface `IPolicyConfig` for this, which has been stable since Windows 7.

// The COM method names of IPolicyConfig are fixed.
#![allow(non_snake_case)]

use std::ffi::c_void;

use windows::core::{interface, IUnknown, IUnknown_Vtbl, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE,
    DEVICE_STATE_ACTIVE, DEVICE_STATE_NOTPRESENT,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    // Only SetDefaultEndpoint is called; the other entries keep the vtable
    // order correct.
    fn GetMixFormat(&self, id: PCWSTR, fmt: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, id: PCWSTR, default: i32, fmt: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, id: PCWSTR, a: *mut c_void, b: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, a: *mut i64, b: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
    fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, id: PCWSTR, fx: i32, key: *const c_void, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, id: PCWSTR, fx: i32, key: *const c_void, value: *mut c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
}

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    /// Friendly name as reported by Windows.
    pub name: String,
    /// Name for the UI: devices sharing a name are numbered ("Name - 1").
    pub label: String,
}

fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let raw: PWSTR = device.GetId()?;
        let id = raw.to_string().unwrap_or_default();
        CoTaskMemFree(Some(raw.0 as *const c_void));
        Ok(id)
    }
}

fn device_name(device: &IMMDevice) -> String {
    unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|value| value.to_string())
            .unwrap_or_else(|_| "(unknown device)".into())
    }
}

/// All active playback devices, sorted alphabetically.
pub fn outputs() -> windows::core::Result<Vec<Device>> {
    unsafe {
        let collection = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut devices = Vec::new();
        for i in 0..collection.GetCount()? {
            let device = collection.Item(i)?;
            let name = device_name(&device);
            devices.push(Device {
                id: device_id(&device)?,
                label: name.clone(),
                name,
            });
        }
        number_duplicates(&mut devices);
        devices.sort_by_cached_key(|d| d.label.to_lowercase());
        Ok(devices)
    }
}

/// Appends " - 1", " - 2", ... to devices that share a name, ordered by ID so
/// the numbers stay the same across runs.
fn number_duplicates(devices: &mut [Device]) {
    for i in 0..devices.len() {
        let mut ids: Vec<&str> = devices
            .iter()
            .filter(|d| d.name == devices[i].name)
            .map(|d| d.id.as_str())
            .collect();
        if ids.len() > 1 {
            ids.sort_unstable();
            let number = ids.iter().position(|id| *id == devices[i].id).unwrap_or(0) + 1;
            devices[i].label = format!("{} - {number}", devices[i].name);
        }
    }
}

fn device_by_id(id: &str) -> windows::core::Result<IMMDevice> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    unsafe { enumerator()?.GetDevice(PCWSTR(wide.as_ptr())) }
}

fn state(id: &str) -> Option<DEVICE_STATE> {
    device_by_id(id).and_then(|device| unsafe { device.GetState() }).ok()
}

/// `true` if the device exists and is currently active (connected and enabled).
pub fn is_available(id: &str) -> bool {
    state(id) == Some(DEVICE_STATE_ACTIVE)
}

/// `true` if Windows no longer knows the device under this ID, as happens
/// when a driver update re-creates the endpoint. Unplugged or disabled
/// devices don't count.
pub fn is_gone(id: &str) -> bool {
    matches!(state(id), None | Some(DEVICE_STATE_NOTPRESENT))
}

/// Friendly name of a device, also for devices that are currently unplugged
/// or disabled.
pub fn name_of(id: &str) -> Option<String> {
    device_by_id(id).ok().map(|device| device_name(&device))
}

/// ID of the current default playback device.
pub fn default_output() -> Option<String> {
    unsafe {
        let device = enumerator().ok()?.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        device_id(&device).ok()
    }
}

/// Sets the default device the same way "Set as Default Device" in the Windows
/// sound control panel does (roles Console + Multimedia). The default
/// communication device is left untouched.
pub fn set_default_output(id: &str) -> windows::core::Result<()> {
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let policy: IPolicyConfig = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
        for role in [eConsole, eMultimedia] {
            policy.SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role).ok()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str) -> Device {
        Device {
            id: id.into(),
            name: name.into(),
            label: name.into(),
        }
    }

    #[test]
    fn numbers_devices_sharing_a_name_by_id() {
        let mut devices = vec![device("{b}", "Monitor"), device("{x}", "TV"), device("{a}", "Monitor")];
        number_duplicates(&mut devices);
        let labels: Vec<&str> = devices.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, ["Monitor - 2", "TV", "Monitor - 1"]);
    }
}
