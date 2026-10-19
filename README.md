# OpenDeck RISE MODE

A native Rust device plugin for using the **RISE MODE Vision 01** stream controller with [OpenDeck](https://github.com/nekename/OpenDeck) on Windows.

This project supports the tested **`0A00:1006`** hardware variant. It is separate from the `0A00:1001` variant listed by the [AKP153 reference plugin](https://github.com/4ndv/opendeck-akp153).

## Supported hardware

| Property | Value |
| --- | --- |
| Device | RISE MODE Vision 01 |
| USB vendor ID | `0x0A00` |
| USB product ID | `0x1006` |
| Control interface | `MI_00` |
| HID usage page | `0xFFA0` |
| HID usage | `0x0001` |
| Layout | 3 rows × 6 columns (18 keys) |
| Tested platform | Windows 11 |

Other hardware variants and operating systems have not been validated. The plugin currently expects one matching device at a time.

## Features

- Automatic USB device discovery, registration, and disconnection handling.
- `KEY_DOWN` and `KEY_UP` events for all 18 keys, with the physical-to-OpenDeck mapping validated on hardware.
- JPEG key images at 96 × 96 pixels, with the validated display orientation.
- Image updates when actions are added or changed, and black keys when actions are removed.
- Startup display clearing to eliminate the device's default logo and residual borders.
- Brightness control from OpenDeck, with values from 0% to 100%.
- Automatic restoration of images, brightness, and button input after USB reconnection or OpenDeck restart.

No custom OpenDeck actions are included: this is a **hardware device plugin**.

## Build

### Requirements

- Windows 11 (tested environment).
- Rust toolchain with the Windows MSVC target.
- OpenDeck with third-party device plugin support.

From the repository root, run:

```powershell
cargo fmt --all -- --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

The resulting Windows executable is:

```text
target/release/opendeck-risemode.exe
```

The repository includes `manifest.json`, which declares `CodePathWin` as `opendeck-risemode.exe`, and the icon in `assets/`. When arranging the plugin files for OpenDeck, keep the manifest's relative executable and asset paths intact. This repository does not currently provide a prebuilt release package.

## Hardware protocol notes

The tested device communicates through its HID control interface. The plugin uses the following reports:

| Command | Use |
| --- | --- |
| `CRT DIS` | Device initialization |
| `CRT LIG` | Initialization and brightness |
| `CRT CLE` | Clear displays at startup |
| `CRT BAT` → JPEG payload → `CRT STP` | Update an individual key image |

On the tested `0A00:1006` device, a `CRT CLE` report containing physical key `7` was observed to clear **all 18 displays**. The plugin deliberately uses that tested report only during device initialization and uses black JPEGs for individual key clearing. Do not assume this behavior applies to other devices.

## Validation

The following behaviors were verified on the physical Vision 01 controller:

- All 18 buttons report presses and releases correctly.
- Images are oriented correctly; adding and removing actions updates the displays.
- Unassigned keys are fully black, without residual borders.
- Brightness changes at 10%, 50%, and 90% work through OpenDeck.
- Two consecutive USB disconnect/reconnect cycles recover the device while OpenDeck remains open.
- Two consecutive OpenDeck restarts recover images, brightness, and button input without reconnecting USB.

These results describe the tested hardware and software environment, not compatibility guarantees for other revisions.

## Development and references

The implementation is written in Rust and uses [`openaction`](https://github.com/OpenActionAPI/rust), [`mirajazz`](https://github.com/4ndv/mirajazz), and Windows HID access.

The [OpenDeck AKP153 plugin](https://github.com/4ndv/opendeck-akp153) provided an architectural and protocol reference. Experimental HID probe programs used during development are not part of the production plugin.

## License

This project is licensed under the [MIT License](LICENSE). Third-party libraries and reference projects retain their own licenses; in particular, `mirajazz` uses MPL-2.0, and the AKP153 reference project is GPL-3.0.
