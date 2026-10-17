use std::{error::Error, io::Cursor, sync::LazyLock};

use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo};
use data_url::DataUrl;
use futures_lite::StreamExt;
use image::{DynamicImage, ImageFormat, imageops::FilterType};
use mirajazz::{
    device::{DeviceQuery, DeviceWatcher, list_devices, new_hid_backend},
    error::MirajazzError,
    types::DeviceLifecycleEvent,
};
use openaction::{
    EventHandlerResult, OUTBOUND_EVENT_MANAGER, OutboundEventManager, SetImageEvent, init_plugin,
};
use simplelog::{ColorChoice, Config, LevelFilter, TermLogger, TerminalMode};
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

const RISEMODE_VID: u16 = 0x0A00;
const RISEMODE_PID: u16 = 0x1006;

const CONTROL_USAGE_PAGE: u16 = 0xFFA0;
const CONTROL_USAGE: u16 = 0x0001;

const DEVICE_ID: &str = "A6-0A00-1006";
const DEVICE_NAME: &str = "RISE MODE Vision 01";

const ROWS: u8 = 3;
const COLUMNS: u8 = 6;
const KEY_COUNT: usize = 18;

const IMAGE_SIZE: u32 = 96;

const INPUT_REPORT_BYTES: usize = 512;
const OUTPUT_REPORT_BYTES: usize = 1025;
const OUTPUT_PAYLOAD_BYTES: usize = 1024;

const QUERIES: [DeviceQuery; 1] = [DeviceQuery::new(
    CONTROL_USAGE_PAGE,
    CONTROL_USAGE,
    RISEMODE_VID,
    RISEMODE_PID,
)];

// Physical key 13 maps to OpenDeck key 0.
const PHYSICAL_TO_OPENDECK: [u8; KEY_COUNT] =
    [4, 10, 16, 3, 9, 15, 2, 8, 14, 1, 7, 13, 0, 6, 12, 5, 11, 17];

// OpenDeck positions are zero-based.
// Image transfers use one-based physical key numbers.
const OPENDECK_TO_PHYSICAL: [u8; KEY_COUNT] = [
    13, 10, 7, 4, 1, 16, 14, 11, 8, 5, 2, 17, 15, 12, 9, 6, 3, 18,
];

// Previously validated initialization reports.
// The first byte is the HID Report ID.
const CRT_DIS: [u8; 9] = [0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x44, 0x49, 0x53];

const CRT_LIG: [u8; 13] = [
    0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x4C, 0x49, 0x47, 0x00, 0x00, 0x00, 0x00,
];

// Exact report from the successful single-command probe.
//
// Although the key field is 7, the RISE MODE Vision 01
// was observed to clear all 18 key displays.
//
// Never substitute 0xFF here without separate validation.
// This command is sent only once per device connection.
const CRT_CLE_OBSERVED_GLOBAL: [u8; 13] = [
    0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x43, 0x4C, 0x45, 0x00, 0x00, 0x00, 0x07,
];

struct ImageUpdate {
    physical_key: u8,
    jpeg: Vec<u8>,
}

static IMAGE_SENDER: LazyLock<Mutex<Option<mpsc::Sender<ImageUpdate>>>> =
    LazyLock::new(|| Mutex::new(None));

// Abort the HID writer when its device task is cancelled.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn encode_jpeg_96(image: DynamicImage) -> Result<Vec<u8>, String> {
    // Effective AKP153 orientation:
    // Rot90 + both-axis mirroring = Rot270.
    let resized = image
        .resize_exact(IMAGE_SIZE, IMAGE_SIZE, FilterType::Lanczos3)
        .rotate270()
        .to_rgb8();

    let mut jpeg = Vec::new();

    DynamicImage::ImageRgb8(resized)
        .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
        .map_err(|error| error.to_string())?;

    if jpeg.is_empty() || jpeg.len() > u16::MAX as usize {
        return Err("JPEG length is outside the tested protocol".into());
    }

    Ok(jpeg)
}

fn prepare_jpeg(data: &str) -> Result<Vec<u8>, String> {
    let url =
        DataUrl::process(data).map_err(|error| format!("Invalid image data URL: {error:?}"))?;

    if url.mime_type().subtype != "jpeg" {
        return Err(format!("Unsupported image MIME type: {}", url.mime_type()));
    }

    let (bytes, _) = url
        .decode_to_vec()
        .map_err(|error| format!("Failed to decode data URL: {error:?}"))?;

    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg)
        .map_err(|error| format!("Invalid JPEG: {error}"))?;

    encode_jpeg_96(image)
}

// Black JPEG uses the same validated image encoder.
fn black_jpeg() -> Result<Vec<u8>, String> {
    encode_jpeg_96(DynamicImage::new_rgb8(IMAGE_SIZE, IMAGE_SIZE))
}

// Queue one image for one OpenDeck position.
async fn queue_image_for_position(
    sender: &mpsc::Sender<ImageUpdate>,
    position: u8,
    jpeg: Vec<u8>,
) -> Result<(), String> {
    let physical_key = OPENDECK_TO_PHYSICAL
        .get(position as usize)
        .copied()
        .ok_or_else(|| format!("Invalid OpenDeck key position: {position}"))?;

    log::info!(
        "Queueing image: OpenDeck {} -> physical {}, {} bytes",
        position,
        physical_key,
        jpeg.len()
    );

    sender
        .send(ImageUpdate { physical_key, jpeg })
        .await
        .map_err(|_| "Image queue unavailable".to_string())
}

// Preserve the previously validated JPEG blanking method.
// Used during initialization and for clear-all requests.
async fn queue_blank_all(sender: &mpsc::Sender<ImageUpdate>) -> Result<(), String> {
    let jpeg = black_jpeg()?;

    for physical_key in OPENDECK_TO_PHYSICAL {
        sender
            .send(ImageUpdate {
                physical_key,
                jpeg: jpeg.clone(),
            })
            .await
            .map_err(|_| "Image queue unavailable".to_string())?;
    }

    log::info!("Queued black images for all {KEY_COUNT} keys");

    Ok(())
}

// Only the previously tested image protocol:
// CRT BAT -> JPEG payload -> CRT STP.
fn make_image_reports(
    physical_key: u8,
    jpeg: &[u8],
) -> Result<Vec<[u8; OUTPUT_REPORT_BYTES]>, &'static str> {
    if !(1..=KEY_COUNT as u8).contains(&physical_key) {
        return Err("Invalid physical key");
    }

    if jpeg.is_empty() || jpeg.len() > u16::MAX as usize {
        return Err("Invalid JPEG length");
    }

    let mut reports = Vec::with_capacity(2 + jpeg.len().div_ceil(OUTPUT_PAYLOAD_BYTES));

    // CRT BAT.
    let mut report = [0u8; OUTPUT_REPORT_BYTES];

    report[1..9].copy_from_slice(b"CRT\0\0BAT");
    report[11] = (jpeg.len() >> 8) as u8;
    report[12] = jpeg.len() as u8;
    report[13] = physical_key;

    reports.push(report);

    // JPEG: 1024 payload bytes per HID report.
    for chunk in jpeg.chunks(OUTPUT_PAYLOAD_BYTES) {
        let mut report = [0u8; OUTPUT_REPORT_BYTES];

        report[1..1 + chunk.len()].copy_from_slice(chunk);

        reports.push(report);
    }

    // CRT STP.
    let mut report = [0u8; OUTPUT_REPORT_BYTES];

    report[1..9].copy_from_slice(b"CRT\0\0STP");

    reports.push(report);

    Ok(reports)
}

struct PluginEvents;

impl openaction::GlobalEventHandler for PluginEvents {
    async fn plugin_ready(&self, _outbound: &mut OutboundEventManager) -> EventHandlerResult {
        log::info!("OpenDeck connected");

        tokio::spawn(async {
            if let Err(error) = watch_devices().await {
                log::error!("Device watcher failed: {error}");
            }
        });

        Ok(())
    }

    async fn set_image(
        &self,
        event: SetImageEvent,
        _outbound: &mut OutboundEventManager,
    ) -> EventHandlerResult {
        if event.device != DEVICE_ID {
            return Ok(());
        }

        if event.controller.as_deref() == Some("Encoder") {
            return Ok(());
        }

        let sender = IMAGE_SENDER.lock().await.clone();

        let Some(sender) = sender else {
            log::warn!("Image event received without active device");
            return Ok(());
        };

        // OpenDeck image event contract:
        // (Some position, Some image) = assign/replace
        // (Some position, None)       = clear one key
        // (None, None)                = clear all keys
        let result = match (event.position, event.image) {
            (Some(position), Some(image)) => match prepare_jpeg(&image) {
                Ok(jpeg) => queue_image_for_position(&sender, position, jpeg).await,
                Err(error) => Err(error),
            },

            (Some(position), None) => {
                log::info!("Clearing OpenDeck key {position}");

                match black_jpeg() {
                    Ok(jpeg) => queue_image_for_position(&sender, position, jpeg).await,
                    Err(error) => Err(error),
                }
            }

            (None, None) => {
                log::info!("OpenDeck requested all images cleared");
                queue_blank_all(&sender).await
            }

            (None, Some(_)) => {
                log::warn!("Ignoring image without a key position");
                Ok(())
            }
        };

        if let Err(error) = result {
            log::error!("Image event failed: {error}");
        }

        Ok(())
    }
}

struct ActionEvents;

impl openaction::ActionEventHandler for ActionEvents {}

async fn deregister_device() {
    let mut manager = OUTBOUND_EVENT_MANAGER.lock().await;

    if let Some(outbound) = manager.as_mut()
        && let Err(error) = outbound.deregister_device(DEVICE_ID.to_string()).await
    {
        log::warn!("Device deregistration: {error}");
    }
}

async fn read_device(info: DeviceInfo) {
    log::info!("Opening RISE MODE control interface");

    let backend = new_hid_backend();

    let devices = match backend.query_devices(&info.id).await {
        Ok(devices) => devices,
        Err(error) => {
            log::error!("Failed to query HID interface: {error}");
            return;
        }
    };

    let Some(device) = devices.into_iter().find(|device| {
        device.matches(
            CONTROL_USAGE_PAGE,
            CONTROL_USAGE,
            RISEMODE_VID,
            RISEMODE_PID,
        )
    }) else {
        log::error!("RISE MODE control interface not found");
        return;
    };

    let (mut reader, mut writer) = match device.open().await {
        Ok(handles) => handles,
        Err(error) => {
            log::error!("Failed to open HID interface: {error}");
            return;
        }
    };

    log::info!("HID interface opened");

    // Preserve the two validated initialization reports.
    let mut report = [0u8; OUTPUT_REPORT_BYTES];

    report[..CRT_DIS.len()].copy_from_slice(&CRT_DIS);

    log::info!("Sending CRT DIS");

    if let Err(error) = writer.write_output_report(&report).await {
        log::error!("CRT DIS failed: {error}");
        return;
    }

    log::info!("CRT DIS submitted");

    report.fill(0);
    report[..CRT_LIG.len()].copy_from_slice(&CRT_LIG);

    log::info!("Sending CRT LIG");

    if let Err(error) = writer.write_output_report(&report).await {
        log::error!("CRT LIG failed: {error}");
        return;
    }

    log::info!("CRT LIG submitted");

    // New: native display clearing at connection startup.
    //
    // The exact report was tested independently.
    // On this hardware it cleared all 18 key displays
    // despite containing physical key 7.
    //
    // Submit it once, before any JPEG transfers.
    // No separate CRT STP is sent for this command.
    report.fill(0);
    report[..CRT_CLE_OBSERVED_GLOBAL.len()].copy_from_slice(&CRT_CLE_OBSERVED_GLOBAL);

    log::info!("Sending CRT CLE (observed global clear)");

    match writer.write_output_report(&report).await {
        Ok(_) => {
            log::info!("CRT CLE submitted");
        }
        Err(error) => {
            // Keep device registration and button input available
            // if native clearing cannot be submitted.
            // The previously tested black JPEG fallback follows.
            log::warn!("CRT CLE failed, continuing with JPEG fallback: {error}");
        }
    }

    log::info!("RISE MODE initialization submitted");

    // Register with the previously validated identity.
    {
        let mut manager = OUTBOUND_EVENT_MANAGER.lock().await;

        let Some(outbound) = manager.as_mut() else {
            log::error!("OpenDeck outbound manager unavailable");
            return;
        };

        if let Err(error) = outbound
            .register_device(
                DEVICE_ID.to_string(),
                DEVICE_NAME.to_string(),
                ROWS,
                COLUMNS,
                0,
                0,
            )
            .await
        {
            log::error!("Failed to register device: {error}");
            return;
        }
    }

    log::info!("RISE MODE registered in OpenDeck");

    // The writer owns the same HID write handle.
    // Button input remains independent below.
    let (image_sender, mut image_receiver) = mpsc::channel::<ImageUpdate>(32);

    let writer_task = tokio::spawn(async move {
        while let Some(update) = image_receiver.recv().await {
            let reports = match make_image_reports(update.physical_key, &update.jpeg) {
                Ok(reports) => reports,
                Err(error) => {
                    log::error!("Image protocol validation: {error}");
                    continue;
                }
            };

            let report_count = reports.len();

            for report in reports {
                if let Err(error) = writer.write_output_report(&report).await {
                    log::error!(
                        "Image transfer failed on physical key {}: {}",
                        update.physical_key,
                        error
                    );
                    return;
                }
            }

            log::info!(
                "Image submitted: physical key {}, {} reports",
                update.physical_key,
                report_count
            );
        }
    });

    let _writer_guard = AbortOnDrop(writer_task);

    *IMAGE_SENDER.lock().await = Some(image_sender.clone());

    // Preserve the already validated black JPEG initialization.
    // CRT CLE removes persistent display borders first.
    // Black JPEGs then leave unassigned keys blank.
    if let Err(error) = queue_blank_all(&image_sender).await {
        log::error!("Initial blank image queue failed: {error}");
    }

    // Ask OpenDeck to redraw configured actions.
    // This is an OpenDeck event, not an HID command.
    {
        let mut manager = OUTBOUND_EVENT_MANAGER.lock().await;

        if let Some(outbound) = manager.as_mut()
            && let Err(error) = outbound.rerender_images(DEVICE_ID.to_string()).await
        {
            log::warn!("Initial image rerender request: {error}");
        }
    }

    log::info!("Image writer ready");
    log::info!("Button reader ready");

    let mut button_states = [false; KEY_COUNT];
    let mut report = [0u8; INPUT_REPORT_BYTES];

    loop {
        report.fill(0);

        let length = match reader.read_input_report(&mut report).await {
            Ok(length) => length,
            Err(error) => {
                log::error!("HID input read failed: {error}");
                break;
            }
        };

        if length < 11 {
            continue;
        }

        if !report[..length].starts_with(b"ACK\0\0OK\0\0") {
            continue;
        }

        let physical_key = report[9];
        let state = report[10];

        if !(1..=KEY_COUNT as u8).contains(&physical_key) {
            continue;
        }

        if state != 0 && state != 1 {
            continue;
        }

        let physical_index = (physical_key - 1) as usize;
        let logical_key = PHYSICAL_TO_OPENDECK[physical_index];

        let pressed = state == 1;
        let logical_index = logical_key as usize;

        if button_states[logical_index] == pressed {
            continue;
        }

        let mut manager = OUTBOUND_EVENT_MANAGER.lock().await;

        let Some(outbound) = manager.as_mut() else {
            log::error!("OpenDeck outbound manager unavailable");
            break;
        };

        let result = if pressed {
            outbound.key_down(DEVICE_ID.to_string(), logical_key).await
        } else {
            outbound.key_up(DEVICE_ID.to_string(), logical_key).await
        };

        if let Err(error) = result {
            log::error!("Failed to send key event: {error}");
            break;
        }

        button_states[logical_index] = pressed;

        log::info!(
            "Button {} -> OpenDeck {}: {}",
            physical_key,
            logical_key,
            if pressed { "DOWN" } else { "UP" }
        );
    }

    *IMAGE_SENDER.lock().await = None;

    deregister_device().await;
    log::info!("RISE MODE input task stopped");
}

async fn watch_devices() -> Result<(), MirajazzError> {
    log::info!(
        "Looking for RISE MODE {:04X}:{:04X}",
        RISEMODE_VID,
        RISEMODE_PID
    );

    let mut watcher = DeviceWatcher::new();
    let mut events = watcher.watch(&QUERIES).await?;

    let mut active_task: Option<JoinHandle<()>> = None;

    let devices = list_devices(&QUERIES).await?;

    if devices.len() == 1 {
        if let Some(device) = devices.into_iter().next() {
            log::info!("RISE MODE detected");

            active_task = Some(tokio::spawn(read_device(device.to_device_info())));
        }
    } else if devices.is_empty() {
        log::info!("RISE MODE not connected");
    } else {
        log::error!("Multiple matching devices");
    }

    log::info!("Device watcher ready");

    while let Some(event) = events.next().await {
        match event {
            DeviceLifecycleEvent::Connected(info) => {
                log::info!("RISE MODE connected");

                if active_task.as_ref().is_some_and(|task| task.is_finished()) {
                    active_task.take();
                }

                if active_task.is_none() {
                    active_task = Some(tokio::spawn(read_device(info)));
                }
            }

            DeviceLifecycleEvent::Disconnected(_) => {
                log::info!("RISE MODE disconnected");

                *IMAGE_SENDER.lock().await = None;

                if let Some(task) = active_task.take() {
                    task.abort();
                }

                deregister_device().await;
            }
        }
    }

    *IMAGE_SENDER.lock().await = None;

    if let Some(task) = active_task {
        task.abort();
    }

    deregister_device().await;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    TermLogger::init(
        LevelFilter::Info,
        Config::default(),
        TerminalMode::Stdout,
        ColorChoice::Never,
    )?;

    log::info!("Starting RISE MODE Vision 01 plugin");

    init_plugin(PluginEvents, ActionEvents).await?;

    log::info!("OpenDeck connection closed");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_mapping_is_reversible() {
        assert_eq!(OPENDECK_TO_PHYSICAL[0], 13);

        for (physical_index, logical_key) in PHYSICAL_TO_OPENDECK.iter().enumerate() {
            assert_eq!(
                OPENDECK_TO_PHYSICAL[*logical_key as usize],
                physical_index as u8 + 1,
            );
        }
    }

    #[test]
    fn image_reports_use_tested_protocol() {
        let jpeg = vec![0xAB; 1025];
        let reports = make_image_reports(13, &jpeg).unwrap();

        assert_eq!(reports.len(), 4);

        assert_eq!(reports[0][0], 0);
        assert_eq!(&reports[0][1..9], b"CRT\0\0BAT");
        assert_eq!(reports[0][11], 0x04);
        assert_eq!(reports[0][12], 0x01);
        assert_eq!(reports[0][13], 13);

        assert_eq!(&reports[1][1..], &jpeg[..1024]);
        assert_eq!(reports[2][1], jpeg[1024]);
        assert!(reports[2][2..].iter().all(|byte| *byte == 0));

        assert_eq!(&reports[3][1..9], b"CRT\0\0STP");

        assert!(make_image_reports(0, &jpeg).is_err());
        assert!(make_image_reports(19, &jpeg).is_err());
        assert!(make_image_reports(13, &[]).is_err());
        assert!(make_image_reports(13, &vec![0; 65536]).is_err());
    }

    #[test]
    fn jpeg_encoding_produces_96x96() {
        let source = DynamicImage::new_rgb8(24, 24);
        let jpeg = encode_jpeg_96(source).unwrap();

        let result = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg).unwrap();

        assert_eq!(result.width(), 96);
        assert_eq!(result.height(), 96);
    }

    #[tokio::test]
    async fn blank_all_queues_18_black_images() {
        let (sender, mut receiver) = mpsc::channel::<ImageUpdate>(KEY_COUNT);

        queue_blank_all(&sender).await.unwrap();

        for expected_key in OPENDECK_TO_PHYSICAL {
            let update = receiver.try_recv().unwrap();

            assert_eq!(update.physical_key, expected_key);

            let image = image::load_from_memory_with_format(&update.jpeg, ImageFormat::Jpeg)
                .unwrap()
                .to_rgb8();

            assert_eq!(image.width(), IMAGE_SIZE);
            assert_eq!(image.height(), IMAGE_SIZE);

            assert!(
                image
                    .pixels()
                    .all(|pixel| { pixel.0.iter().all(|channel| *channel < 8) })
            );
        }

        assert!(receiver.try_recv().is_err());
    }
}
