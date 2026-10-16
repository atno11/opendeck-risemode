use std::{error::Error, io::Cursor, time::Duration};

use async_hid::{AsyncHidRead, AsyncHidWrite};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use mirajazz::device::{DeviceQuery, list_devices};
use tokio::time::timeout;

const VID: u16 = 0x0A00;
const PID: u16 = 0x1006;

const USAGE_PAGE: u16 = 0xFFA0;
const USAGE: u16 = 0x0001;

const QUERIES: [DeviceQuery; 1] = [DeviceQuery::new(USAGE_PAGE, USAGE, VID, PID)];

const IMAGE_SIZE: u32 = 96;

// The HID API expects a Report ID byte before the
// 1024 bytes observed in the USB capture.
const OUTPUT_REPORT_BYTES: usize = 1025;
const OUTPUT_PAYLOAD_BYTES: usize = 1024;

const INPUT_REPORT_BYTES: usize = 513;

// Physical key 13 is logical position (0,0).
const TARGET_KEY: u8 = 13;

const MAX_INPUT_REPORTS: usize = 12;
const READ_TIMEOUT: Duration = Duration::from_secs(20);

fn create_jpeg() -> Result<Vec<u8>, Box<dyn Error>> {
    // A simple test image generated entirely in Rust.
    // Dark background with a bright green square.
    let image = RgbImage::from_fn(IMAGE_SIZE, IMAGE_SIZE, |x, y| {
        if (16..80).contains(&x) && (16..80).contains(&y) {
            Rgb([0, 240, 160])
        } else {
            Rgb([12, 24, 36])
        }
    });

    let mut jpeg = Vec::new();

    DynamicImage::ImageRgb8(image).write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)?;

    if jpeg.is_empty() || jpeg.len() > u16::MAX as usize {
        return Err("Invalid JPEG length".into());
    }

    Ok(jpeg)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);

    if args.next().as_deref() != Some("--send-image-once") || args.next().is_some() {
        return Err("Explicit --send-image-once argument required".into());
    }

    println!("=== RISE MODE VISION 01 IMAGE TEST ===");
    println!("Target: 0A00:1006 / FFA0:0001");
    println!("Target key: physical 13 / OpenDeck (0,0)");
    println!("Image: JPEG 96x96");
    println!("Allowed commands: CRT BAT and CRT STP");
    println!("No other commands will be sent");
    println!();

    // Prepare the complete JPEG before opening USB.
    let jpeg = create_jpeg()?;

    println!("[PASS] JPEG generated: {} bytes", jpeg.len());

    let devices = list_devices(&QUERIES).await?;

    if devices.len() != 1 {
        return Err(format!("Expected one RISE MODE, found {}", devices.len()).into());
    }

    let device = devices.into_iter().next().ok_or("Device disappeared")?;

    // Verify the exact HID collection.
    if !device.matches(USAGE_PAGE, USAGE, VID, PID) {
        return Err("Unexpected HID identity".into());
    }

    println!("[PASS] Unique control interface identified");

    // Opening is not an initialization command.
    let (mut reader, mut writer) = device.open().await?;

    println!("[PASS] HID read/write handles opened");
    println!();

    let length = jpeg.len();

    // Step 1: CRT BAT
    //
    // Wire payload:
    // CRT 00 00 BAT 00 00 SIZE_HI SIZE_LO KEY
    //
    // Byte zero in the API buffer is the Report ID.
    let mut packet = [0u8; OUTPUT_REPORT_BYTES];

    packet[1..9].copy_from_slice(b"CRT\0\0BAT");
    packet[11] = (length >> 8) as u8;
    packet[12] = length as u8;
    packet[13] = TARGET_KEY;

    println!("[ACTION] CRT BAT");

    writer.write_output_report(&packet).await?;

    println!("[PASS] Image header submitted");

    // Step 2: JPEG payload.
    //
    // Each report contains:
    // - 1 Report ID byte (zero)
    // - Up to 1024 JPEG bytes
    // - Zero padding in the final report
    let chunks = jpeg.chunks(OUTPUT_PAYLOAD_BYTES);

    println!("[ACTION] Sending JPEG data");

    let mut sent = 0usize;

    for chunk in chunks {
        packet.fill(0);
        packet[1..1 + chunk.len()].copy_from_slice(chunk);

        writer.write_output_report(&packet).await?;

        sent += 1;
    }

    println!("[PASS] JPEG transmitted in {sent} reports");

    // Step 3: CRT STP
    //
    // This is the final write operation.
    packet.fill(0);
    packet[1..9].copy_from_slice(b"CRT\0\0STP");

    println!("[ACTION] CRT STP");

    writer.write_output_report(&packet).await?;

    println!("[PASS] Image update submitted");
    println!();
    println!("=== BUTTON INPUT OBSERVATION ===");
    println!("Check the image on the physical key (0,0).");
    println!("Press and release that key a few times.");
    println!();

    // From here onward, HID input only.
    // No additional output reports are submitted.
    for index in 0..MAX_INPUT_REPORTS {
        let mut buffer = [0u8; INPUT_REPORT_BYTES];

        let result = timeout(READ_TIMEOUT, reader.read_input_report(&mut buffer)).await;

        let received = match result {
            Ok(Ok(length)) => length,

            Ok(Err(error)) => {
                println!("[ERROR] HID input: {error}");
                return Err(error.into());
            }

            Err(_) => {
                println!("[INFO] No input report before timeout");
                break;
            }
        };

        if received == 0 {
            continue;
        }

        let prefix = buffer[..received.min(16)]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");

        println!("REPORT {} | {} bytes | {}", index + 1, received, prefix);

        if received >= 11 && buffer.starts_with(b"ACK\0\0OK\0\0") {
            let key = buffer[9];
            let state = buffer[10];

            let event = match state {
                0 => "UP",
                1 => "DOWN",
                _ => "UNKNOWN",
            };

            println!("BUTTON: physical={key} state={event}");

            if key == TARGET_KEY {
                println!("[MATCH] OpenDeck (0,0): {event}");
            }
        }
    }

    println!();
    println!("[DONE] Test finished");
    println!("[INFO] No additional HID commands sent");

    Ok(())
}
