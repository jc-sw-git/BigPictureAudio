# Changelog

The section for a version is used as the release notes when its tag is
pushed, so add it before tagging.

## v0.1.2

- Fixed: "Start with Windows" showed as enabled even when the autostart entry pointed to a renamed or moved copy of the app, which then didn't start at login. The option is now only checked if it starts this exact executable; clicking it updates the path.

## v0.1.1

- Fixed: the tray menu closed after changing a setting. It now stays open, so several settings can be changed in one go.

## v0.1.0

First release.

- Switches the default audio output device when Steam enters Big Picture mode
- Detects Big Picture in every Steam client language (window titles are read from Steam's own localization files)
- Configurable behavior when leaving Big Picture: don't switch (default), switch back to the previous device, or switch to a specific device
- Respects manual changes: if you switch devices yourself during Big Picture, the app stops switching for that session; optionally also when leaving ("Not if I switched manually", on by default)
- Waits for the Big Picture device if it isn't available yet (e.g. TV audio over HDMI) and switches as soon as it appears; a device for leaving is waited for up to 30 seconds
- Finds a device again after its ID changed (e.g. after a driver update), as long as its name is unique
- Tray menu shows which device follows when leaving Big Picture
- Devices are sorted alphabetically; devices sharing a name are numbered
- UI in 13 languages, following the Windows display language by default
- Optional "Start with Windows"
- Recovers after a crash or reboot during Big Picture

Known issue: the tray menu closes after changing a setting – fixed in v0.1.1.
