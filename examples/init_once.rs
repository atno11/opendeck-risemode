use std::{
    error::Error,
    time::{Duration, Instant},
};

use hidapi::{HidApi, HidDevice};

const VID: u16 = 0x0A00;
const PID: u16 = 0x1006;

const USAGE_PAGE: u16 = 0xFFA0;
const USAGE: u16 = 0x0001;

const OUTPUT_REPORT_BYTES: usize = 1025;
const INPUT_REPORT_BYTES: usize = 513;

const MAX_REPORTS: usize = 12;
const READ_DURATION: Duration = Duration::from_secs(30);

// Exact initialization commands from mirajazz.
// Byte zero is the HID Report ID.
const CRT_DIS: [u8; 9] = [0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x44, 0x49, 0x53];

const CRT_LIG: [u8; 13] = [
    0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x4C, 0x49, 0x47, 0x00, 0x00, 0x00, 0x00,
];

fn send_command(device: &HidDevice, name: &str, command: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut report = [0u8; OUTPUT_REPORT_BYTES];

    report[..command.len()].copy_from_slice(command);

    println!("[ACTION] Sending {name}");

    let written = device.write(&report)?;

    if written != OUTPUT_REPORT_BYTES {
        return Err(format!(
            "Incomplete HID write for {name}: \
             {written}/{OUTPUT_REPORT_BYTES} bytes"
        )
        .into());
    }

    println!("[PASS] {name} submitted");

    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.len() != 1 || args[0] != "--init-once" {
        return Err("Explicit --init-once argument required".into());
    }

    println!("=== RISE MODE VISION 01 INITIALIZATION TEST ===");
    println!("Target: 0A00:1006 / FFA0:0001");
    println!("Output report size: 1025 bytes");
    println!("Authorized commands: CRT DIS + CRT LIG");
    println!("Maximum HID writes: 2");
    println!();

    let api = HidApi::new()?;

    let candidates: Vec<_> = api
        .device_list()
        .filter(|info| {
            info.vendor_id() == VID
                && info.product_id() == PID
                && info.usage_page() == USAGE_PAGE
                && info.usage() == USAGE
                && info.interface_number() == 0
        })
        .collect();

    if candidates.len() != 1 {
        return Err(format!(
            "Expected exactly one MI_00 interface, found {}",
            candidates.len()
        )
        .into());
    }

    println!("[PASS] Unique MI_00 interface identified");

    let device = candidates[0].open_device(&api)?;

    println!("[PASS] HID handle opened");
    println!();

    // Exactly two output reports.
    // No additional HID commands are sent anywhere.
    send_command(&device, "CRT DIS", &CRT_DIS)?;
    send_command(&device, "CRT LIG", &CRT_LIG)?;

    println!();
    println!("[PASS] Initialization sequence submitted");
    println!();
    println!("=== BUTTON INPUT OBSERVATION ===");
    println!("Press and release physical key (0,0).");
    println!("Listening for up to 30 seconds.");
    println!();

    let deadline = Instant::now() + READ_DURATION;
    let mut reports = 0usize;

    while Instant::now() < deadline && reports < MAX_REPORTS {
        let mut buffer = [0u8; INPUT_REPORT_BYTES];

        // HID input only.
        let length = device.read_timeout(&mut buffer, 500)?;

        if length == 0 {
            continue;
        }

        reports += 1;

        let data = &buffer[..length];

        let prefix = data
            .iter()
            .take(32)
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        println!("REPORT {reports}: {length} bytes");
        println!("DATA: {prefix}");

        let header = b"ACK\0\0OK\0\0";

        // Account for optional leading Report ID.
        let offset = if data.starts_with(header) {
            Some(0)
        } else if data.get(1..).is_some_and(|bytes| bytes.starts_with(header)) {
            Some(1)
        } else {
            None
        };

        if let Some(offset) = offset
            && length >= offset + 11
        {
            let key = data[offset + 9];
            let state = data[offset + 10];

            let event = match state {
                0 => "UP",
                1 => "DOWN",
                _ => "UNKNOWN",
            };

            println!("BUTTON: physical={key}, state={event}");

            if key == 13 {
                println!("[MATCH] OpenDeck (0,0): {event}");
            }
        }

        println!();
    }

    if reports == 0 {
        println!("[INFO] No HID input reports received");
    }

    println!("[DONE] Captured {reports} reports");
    println!("[INFO] No additional HID writes performed");

    Ok(())
}
