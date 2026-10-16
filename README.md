

# OpenDeck RISE MODE

Native Rust hardware plugin for RISE MODE Stream Deck
controllers running in OpenDeck.

## Hardware

- Vendor ID: `0x0A00`
- Product ID: `0x1006`
- Control interface: `MI_00`
- Usage page: `0xFFA0`
- Usage: `0x0001`

## Technology

- Rust
- OpenDeck
- openaction
- mirajazz

## Status

Initial plugin foundation.

Implemented:
- OpenDeck plugin entry point
- RISE MODE HID identification
- USB connection/disconnection monitoring
- Passive device enumeration

Not yet implemented:
- Operational device registration
- Button events
- Display image transfers
- Brightness controls

The plugin does not currently send HID reports.

## Development

Windows 11 with Rust MSVC toolchain.

```powershell
cargo fmt --all -- --check
cargo check
cargo test
```

## References

- https://github.com/4ndv/opendeck-akp153
- https://github.com/4ndv/mirajazz
- https://github.com/nekename/OpenDeck

## License

MIT.

The project uses mirajazz under MPL-2.0.
The AKP153 GPL-3.0 implementation is used as an
architectural reference, not copied into this project.
