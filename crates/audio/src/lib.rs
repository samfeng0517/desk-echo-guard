//! Audio device I/O behind a small interface (`io_backend.py`, `sim.py`).
//!
//! Only the WASAPI backend touches real hardware; the rest of the program
//! talks to [`InputStream`] / [`OutputStream`], so it can be driven by the
//! simulated backend in tests and on machines without audio devices.
//! All streams are mono f32 at 48 kHz; multi-channel devices are down-mixed
//! on input and duplicated on output.

use std::sync::Arc;

use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// capture endpoint
    Mic,
    /// render endpoint (loopback capture works on these)
    Speaker,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub kind: DeviceKind,
    pub name: String,
    pub id: String,
    pub channels: u16,
    pub default: bool,
}

pub trait InputStream: Send {
    /// Block until `out` is filled.
    fn read(&mut self, out: &mut [f32]) -> Result<()>;
}

pub trait OutputStream: Send {
    /// Block until `x` has been queued to the device.
    fn write(&mut self, x: &[f32]) -> Result<()>;
}

pub trait AudioBackend: Send + Sync {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    /// `name`: case-insensitive substring (or exact id); `None` = system
    /// default.  `loopback`: capture what a *speaker* is playing.
    fn open_input(
        &self,
        name: Option<&str>,
        loopback: bool,
        blocksize: usize,
    ) -> Result<Box<dyn InputStream>>;
    fn open_output(&self, name: Option<&str>, blocksize: usize) -> Result<Box<dyn OutputStream>>;
    /// Simulated backends may run faster than real time; the elastic
    /// buffers' wall-clock interpolation must use the same time scale.
    fn speed(&self) -> f64 {
        1.0
    }
}

/// Pick a device by case-insensitive name substring (or exact id); several
/// hits -> the shortest name ("CABLE Input" over "CABLE Input 16ch ...").
pub fn match_device<'a>(
    items: &'a [DeviceInfo],
    query: &str,
    what: &str,
) -> Result<&'a DeviceInfo> {
    let q = query.to_lowercase();
    if let Some(d) = items
        .iter()
        .find(|d| d.name.to_lowercase() == q || d.id == query)
    {
        return Ok(d);
    }
    let hits = items.iter().filter(|d| d.name.to_lowercase().contains(&q));
    match hits.min_by_key(|d| d.name.chars().count()) {
        Some(d) => Ok(d),
        None => {
            let names: Vec<&str> = items.iter().map(|d| d.name.as_str()).collect();
            bail!(
                "no {what} matches {query:?}. Available:\n  {}",
                names.join("\n  ")
            )
        }
    }
}

/// The real device backend of this platform (WASAPI on Windows).
pub fn default_backend() -> Result<Arc<dyn AudioBackend>> {
    #[cfg(windows)]
    {
        Ok(Arc::new(wasapi::WasapiBackend::new()?))
    }
    #[cfg(not(windows))]
    {
        bail!("no audio backend on this platform (WASAPI is Windows-only)")
    }
}

pub mod sim;
pub mod stream;
#[cfg(windows)]
pub mod wasapi;

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str) -> DeviceInfo {
        DeviceInfo {
            kind: DeviceKind::Speaker,
            name: name.into(),
            id: name.into(),
            channels: 2,
            default: false,
        }
    }

    #[test]
    fn match_prefers_exact_then_shortest() {
        let d = [
            dev("CABLE In 16ch (VB-Audio Virtual Cable)"),
            dev("CABLE Input (VB-Audio Virtual Cable)"),
            dev("Speakers (Realtek)"),
        ];
        assert_eq!(
            match_device(&d, "cable in", "speaker").unwrap().name,
            d[1].name
        );
        assert_eq!(
            match_device(&d, "realtek", "speaker").unwrap().name,
            d[2].name
        );
        assert!(match_device(&d, "usb", "speaker").is_err());
    }
}
