use std::{error::Error, time::Duration};

use async_hid::AsyncHidRead;
use mirajazz::device::{DeviceQuery, list_devices};
use tokio::time::timeout;

const QUERIES: [DeviceQuery; 1] = [DeviceQuery::new(0xFFA0, 0x0001, 0x0A00, 0x1006)];

// Measured Windows HID InputReportByteLength.
// Includes the Report ID byte.
const INPUT_REPORT_BYTES: usize = 513;

const MAX_REPORTS: usize = 12;
const READ_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if std::env::args().skip(1).collect::<Vec<_>>() != ["--read-only"] {
        return Err("Explicit --read-only argument required".into());
    }

    println!("=== RISE MODE BUTTON INPUT PROBE ===");
    println!("Target: 0A00:1006 / FFA0:0001");
    println!("Read-only HID session");
    println!("No output or feature reports will be sent");

    let devices = list_devices(&QUERIES).await?;

    if devices.len() != 1 {
        return Err(format!("Expected exactly one device, found {}", devices.len()).into());
    }

    let device = devices
        .into_iter()
        .next()
        .ok_or("Device disappeared from enumeration")?;

    println!("[PASS] Unique control collection found");

    // Open a read-only handle.
    // Never call Device::connect or initialize().
    let mut reader = device.open_readable().await?;

    println!("[PASS] Read-only handle opened");
    println!("Press and release a few buttons individually.");
    println!("Capture ends after 12 reports or a read timeout.");

    for index in 0..MAX_REPORTS {
        let mut buffer = [0u8; INPUT_REPORT_BYTES];

        let result = timeout(READ_TIMEOUT, reader.read_input_report(&mut buffer)).await;

        let length = match result {
            Ok(Ok(length)) => length,

            Ok(Err(error)) => {
                return Err(error.into());
            }

            Err(_) => {
                println!("[INFO] No input report before timeout");
                break;
            }
        };

        if length == 0 {
            println!("[INFO] Empty input report");
            continue;
        }

        let visible_length = length.min(buffer.len());

        let prefix = buffer[..visible_length.min(32)]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        let nonzero = buffer[..visible_length]
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte != 0)
            .take(48)
            .map(|(offset, byte)| format!("{offset}:{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        println!();
        println!("REPORT {} | received {} bytes", index + 1, length);
        println!("PREFIX: {prefix}");
        println!("NONZERO: {nonzero}");
    }

    println!();
    println!("[DONE] Read-only capture finished");

    Ok(())
}
