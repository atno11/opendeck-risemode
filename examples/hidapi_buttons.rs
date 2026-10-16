use std::{
    error::Error,
    time::{Duration, Instant},
};

use hidapi::HidApi;

const VID: u16 = 0x0A00;
const PID: u16 = 0x1006;
const USAGE_PAGE: u16 = 0xFFA0;
const USAGE: u16 = 0x0001;

const INPUT_LENGTH: usize = 513;
const MAX_REPORTS: usize = 12;
const DURATION: Duration = Duration::from_secs(30);

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args != ["--read-only"] {
        return Err("Required argument: --read-only".into());
    }

    println!("=== RISE MODE HIDAPI INPUT TEST ===");
    println!("Target: 0A00:1006 / FFA0:0001");
    println!("Read-only operations");
    println!("No output or feature reports");
    println!();

    let api = HidApi::new()?;

    let candidates: Vec<_> = api
        .device_list()
        .filter(|device| {
            device.vendor_id() == VID
                && device.product_id() == PID
                && device.usage_page() == USAGE_PAGE
                && device.usage() == USAGE
        })
        .collect();

    println!("[INFO] Matching HID collections: {}", candidates.len());

    if candidates.len() != 1 {
        return Err("Expected exactly one control interface".into());
    }

    let info = candidates[0];

    println!("[PASS] Unique interface identified");
    println!("[INFO] Interface number: {}", info.interface_number());

    // Open the exact enumerated HID path.
    let device = info.open_device(&api)?;

    println!("[PASS] HID handle opened");
    println!();
    println!("Press and release physical key (0,0).");
    println!("Reading input for up to 30 seconds.");
    println!();

    let deadline = Instant::now() + DURATION;
    let mut reports = 0usize;

    while Instant::now() < deadline && reports < MAX_REPORTS {
        let mut buffer = [0u8; INPUT_LENGTH];

        // Read only. No HID write methods are used.
        let length = device.read_timeout(&mut buffer, 1000)?;

        if length == 0 {
            continue;
        }

        reports += 1;

        let prefix = buffer[..length.min(32)]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        println!("REPORT {reports}: {length} bytes");
        println!("DATA: {prefix}");

        // Different Windows backends may expose the
        // optional zero Report ID byte differently.
        let header = b"ACK\0\0OK\0\0";

        let offset = if buffer[..length].starts_with(header) {
            Some(0)
        } else if length > 1 && buffer[1..length].starts_with(header) {
            Some(1)
        } else {
            None
        };

        if let Some(offset) = offset {
            if length >= offset + 11 {
                let key = buffer[offset + 9];
                let state = buffer[offset + 10];

                println!(
                    "BUTTON: physical={key}, state={}",
                    match state {
                        0 => "UP",
                        1 => "DOWN",
                        _ => "UNKNOWN",
                    }
                );

                if key == 13 {
                    println!("[MATCH] OpenDeck key (0,0)");
                }
            }
        }

        println!();
    }

    if reports == 0 {
        println!("[INFO] No HID input reports received");
    }

    println!("[DONE] Captured {reports} reports");
    println!("[INFO] No HID writes performed");

    Ok(())
}
