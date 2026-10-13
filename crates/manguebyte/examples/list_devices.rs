//! Prints every audio device on the system and what it can do.
//!
//! ```text
//! cargo run --example list_devices
//! ```

use manguebyte::devices::{DeviceError, DeviceInfo, input_devices, output_devices};

fn main() -> Result<(), DeviceError> {
    print("Output devices", &output_devices()?);
    print("Input devices", &input_devices()?);
    Ok(())
}

fn print(title: &str, devices: &[DeviceInfo]) {
    println!("{title}:");
    if devices.is_empty() {
        println!("  (none)");
    }
    for device in devices {
        let marker = if device.is_default { " (default)" } else { "" };
        println!("  {}{marker}", device.name);
        println!("    id: {}", device.id);
        if let (Some(rate), Some(channels)) = (device.default_sample_rate, device.default_channels)
        {
            println!("    prefers {rate} Hz, {channels} channels");
        }
        for config in &device.configs {
            let rates = if config.min_sample_rate == config.max_sample_rate {
                format!("{} Hz", config.min_sample_rate)
            } else {
                format!("{}-{} Hz", config.min_sample_rate, config.max_sample_rate)
            };
            let buffer = match config.buffer_size {
                Some((min, max)) => format!(", buffer {min}-{max} frames"),
                None => String::new(),
            };
            println!("    {} channels, {rates}{buffer}", config.channels);
        }
    }
    println!();
}
