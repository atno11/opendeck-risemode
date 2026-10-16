use std::{error::Error, time::Duration};

use async_hid::{AsyncHidRead, AsyncHidWrite};
use mirajazz::device::{DeviceQuery, list_devices};
use tokio::time::timeout;

const RISEMODE_VID: u16 = 0x0A00;
const RISEMODE_PID: u16 = 0x1006;

const QUERIES: [DeviceQuery; 1] = [DeviceQuery::new(0xFFA0, 0x0001, RISEMODE_VID, RISEMODE_PID)];

// Previously measured HID report lengths.
// Both include the Report ID byte.
const INPUT_REPORT_BYTES: usize = 513;
const OUTPUT_REPORT_BYTES: usize = 1025;

// AKP153/Mirajazz initialization command: CRT DIS.
// This is the ONLY command authorized for this test.
const CRT_DIS: [u8; 9] = [
    0x00, // Report ID
    0x43, // C
    0x52, // R
    0x54, // T
    0x00, 0x00, 0x44, // D
    0x49, // I
    0x53, // S
];

const MAX_REPORTS: usize = 12;
const READ_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.len() != 1 || args[0] != "--send-crt-dis-once" {
        return Err("Explicit --send-crt-dis-once argument required".into());
    }

    println!("=== RISE MODE CRT DIS TEST ===");
    println!("Target: 0A00:1006 / FFA0:0001");
    println!("Maximum output reports: 1");
    println!("Command: CRT DIS");
    println!("Output report length: 1025 bytes");
    println!("No additional initialization commands");
    println!();

    // Identify the exact target device.
    let devices = list_devices(&QUERIES).await?;

    if devices.len() != 1 {
        return Err(format!("Expected exactly one device, found {}", devices.len()).into());
    }

    let device = devices
        .into_iter()
        .next()
        .ok_or("Target device disappeared")?;

    if device.vendor_id != RISEMODE_VID
        || device.product_id != RISEMODE_PID
        || device.usage_page != 0xFFA0
        || device.usage_id != 0x0001
    {
        return Err("Unexpected HID device identity".into());
    }

    println!("[PASS] Unique RISE MODE device selected");

    // Opens read/write handles, without initializing
    // the device or transferring reports.
    let (mut reader, mut writer) = device.open().await?;

    println!("[PASS] HID handles opened");

    // Build exactly one 1025-byte HID output report.
    // All bytes after CRT DIS remain zero.
    let mut output = [0u8; OUTPUT_REPORT_BYTES];
    output[..CRT_DIS.len()].copy_from_slice(&CRT_DIS);

    println!();
    println!("[ACTION] Sending CRT DIS once");

    // The ONLY HID write operation in this program.
    writer.write_output_report(&output).await?;

    println!("[PASS] Single CRT DIS output report submitted");
    println!();
    println!("Press and release individual buttons now.");
    println!("Waiting for up to 12 input reports.");
    println!();

    // Passive observation after the single write.
    for index in 0..MAX_REPORTS {
        let mut buffer = [0u8; INPUT_REPORT_BYTES];

        let result = timeout(READ_TIMEOUT, reader.read_input_report(&mut buffer)).await;

        let length = match result {
            Ok(Ok(length)) => length,

            Ok(Err(error)) => {
                println!("[ERROR] HID input read failed");
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

        println!("REPORT {} | received {} bytes", index + 1, length);
        println!("PREFIX: {prefix}");
        println!("NONZERO: {nonzero}");
        println!();
    }

    println!("[DONE] Controlled test finished");
    println!("[INFO] No additional HID commands sent");

    Ok(())
}
