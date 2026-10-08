# Big Picture Audio

A small Windows tray app that switches the default audio output device when
Steam enters Big Picture mode and, optionally, switches back to the previous
device (or any other device) when you leave it. It does for audio what Steam
already does for displays.

## Installation

1. Download `big-picture-audio.exe` from the
   [latest release](https://github.com/jc-sw-git/BigPictureAudio/releases/latest).
   It's a single self-contained executable, so there's no installer.
2. Move it to a permanent location, e.g. `%LOCALAPPDATA%\Programs\BigPictureAudio\`.
3. Run it. A tray icon appears in the notification area.
4. Click the tray icon, pick your device under **Output device for Big Picture**
   and enable **Start with Windows**.

**Start with Windows** stores the current path of the executable. If you move
the `.exe` later, toggle the option off and on again.

The executable is not code-signed, so Windows SmartScreen may show a warning on
first launch (**More info → Run anyway**).

### Uninstall

Disable **Start with Windows**, exit the app, then delete the `.exe` and
`%APPDATA%\BigPictureAudio`.

## Usage

Click the tray icon:

| Menu entry | Description |
|---|---|
| **Output device in Big Picture** | Device to switch to when Big Picture starts. **Don't switch** disables switching. |
| **When leaving Big Picture** | **Don't switch** (default) keeps the current device. **Previous device** switches back to the device that was active before Big Picture. You can also pick any specific device. |
| **Language** | UI language. **Automatic (Windows)** (default) follows the Windows display language. |
| **Start with Windows** | Adds or removes an entry under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. |
| **Open log folder** | Opens `%APPDATA%\BigPictureAudio` (`config.ini`, `log.txt`). |
| **Exit** | Quits the app. If Big Picture is active, the *When leaving* action runs first. |

**Disconnected devices:** If the Big Picture device isn't available yet when
Big Picture starts (e.g. TV audio over HDMI only appears once the TV is on),
the app keeps checking every second and switches as soon as the device shows
up. A device chosen for leaving Big Picture is waited for up to 30 seconds.

**Languages:** English, French, Spanish, Italian, German, Polish, Dutch,
Danish, Swedish, Norwegian, Finnish, Portuguese and Turkish. Other Windows
languages fall back to English.

## How it works

- **Detection:** Once per second the app looks for a visible `SDL_app` window
  owned by `steamwebhelper.exe` whose title matches one of the Big Picture
  window titles. Steam localizes that title (in German it's
  `Big-Picture-Modus`, for example), so the titles for all languages are read
  from Steam's own localization files (`steamui/localization/steamui_*-json.js`,
  key `SP_WindowTitle_BigPicture`). Tools that only look for the English title
  `Steam Big Picture Mode` never detect Big Picture on a non-English client.
- **Switching:** Uses the undocumented `IPolicyConfig` COM interface (like
  EarTrumpet or SoundSwitch) and sets the Console and Multimedia roles, which
  is the same as "Set as Default Device" in Windows. The default communication
  device is left untouched.
- **Leaving:** The previous device is remembered at the moment Big Picture
  starts. It is persisted to `config.ini` along with the running session, so
  the *When leaving* action still runs after a crash or reboot.

## Building

Requires a Rust toolchain on Windows.

```powershell
cargo build --release
```

The binary ends up in `target\release\big-picture-audio.exe`.

```powershell
cargo test                              # unit tests
cargo test -- --ignored --nocapture     # smoke test against the real system
```

Pushing a `v*` tag builds the executable via GitHub Actions and attaches it to
a new release.

## License

[MIT](LICENSE)
