use std::error::Error;

use async_hid::{AsyncHidRead, AsyncHidWrite, DeviceInfo};
use futures_lite::StreamExt;
use mirajazz::{
    device::{DeviceQuery, DeviceWatcher, list_devices, new_hid_backend},
    error::MirajazzError,
    types::DeviceLifecycleEvent,
};
use openaction::{EventHandlerResult, OUTBOUND_EVENT_MANAGER, OutboundEventManager, init_plugin};
use simplelog::{ColorChoice, Config, LevelFilter, TermLogger, TerminalMode};
use tokio::task::JoinHandle;

const RISEMODE_VID: u16 = 0x0A00;
const RISEMODE_PID: u16 = 0x1006;

const CONTROL_USAGE_PAGE: u16 = 0xFFA0;
const CONTROL_USAGE: u16 = 0x0001;

const DEVICE_ID: &str = "A6-0A00-1006";
const DEVICE_NAME: &str = "RISE MODE Vision 01";

const ROWS: u8 = 3;
const COLUMNS: u8 = 6;
const KEY_COUNT: usize = 18;

const INPUT_REPORT_BYTES: usize = 512;
const OUTPUT_REPORT_BYTES: usize = 1025;

const QUERIES: [DeviceQuery; 1] = [DeviceQuery::new(
    CONTROL_USAGE_PAGE,
    CONTROL_USAGE,
    RISEMODE_VID,
    RISEMODE_PID,
)];

// Physical key 13 maps to OpenDeck key 0.
const PHYSICAL_TO_OPENDECK: [u8; KEY_COUNT] =
    [4, 10, 16, 3, 9, 15, 2, 8, 14, 1, 7, 13, 0, 6, 12, 5, 11, 17];

// AKP153 initialization commands.
// Byte zero is the HID Report ID.
const CRT_DIS: [u8; 9] = [0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x44, 0x49, 0x53];

const CRT_LIG: [u8; 13] = [
    0x00, 0x43, 0x52, 0x54, 0x00, 0x00, 0x4C, 0x49, 0x47, 0x00, 0x00, 0x00, 0x00,
];

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
}

struct ActionEvents;

impl openaction::ActionEventHandler for ActionEvents {}

async fn deregister_device() {
    let mut manager = OUTBOUND_EVENT_MANAGER.lock().await;

    if let Some(outbound) = manager.as_mut() {
        if let Err(error) = outbound.deregister_device(DEVICE_ID.to_string()).await {
            log::warn!("Device deregistration: {error}");
        }
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

    // Initialize the controller once per connection.
    // Exactly two output reports are sent.
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
    log::info!("RISE MODE initialization submitted");

    // Register the controller with OpenDeck.
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

        // Header confirmed by the USB capture.
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

                if let Some(task) = active_task.take() {
                    task.abort();
                }

                deregister_device().await;
            }
        }
    }

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
