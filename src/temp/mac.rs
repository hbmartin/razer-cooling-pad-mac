//! macOS CPU temperature reading.
//!
//! Primary source: SMC temperature sensors exposed through the private
//! `IOHIDEventSystemClient` API (usage page 0xff00, usage 5) — the same
//! mechanism used by macmon/stats/socpowerbud. Works without root, but the
//! API is private and could change, so a public fallback maps
//! `NSProcessInfo.thermalState` onto coarse temperature estimates.

use anyhow::{Result, bail};
use core_foundation::array::CFArray;
use core_foundation::base::{CFRelease, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;

use super::Reading;

// kHIDPage_AppleVendor / kHIDUsage_AppleVendor_TemperatureSensor
const APPLE_VENDOR_PAGE: i32 = 0xff00;
const TEMPERATURE_SENSOR_USAGE: i32 = 5;
// IOHIDEventTypeTemperature; field base = type << 16
const EVENT_TYPE_TEMPERATURE: i64 = 15;
const TEMPERATURE_FIELD: u32 = (EVENT_TYPE_TEMPERATURE as u32) << 16;

#[repr(C)]
struct Opaque {
    _private: [u8; 0],
}
type IOHIDEventSystemClientRef = *mut Opaque;
type IOHIDServiceClientRef = *mut Opaque;
type IOHIDEventRef = *mut Opaque;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: *const c_void) -> IOHIDEventSystemClientRef;
    fn IOHIDEventSystemClientSetMatching(
        client: IOHIDEventSystemClientRef,
        matching: CFDictionaryRef,
    ) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: IOHIDEventSystemClientRef) -> *const c_void;
    fn IOHIDServiceClientCopyProperty(
        service: IOHIDServiceClientRef,
        key: CFStringRef,
    ) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(
        service: IOHIDServiceClientRef,
        event_type: i64,
        options: i32,
        timestamp: i64,
    ) -> IOHIDEventRef;
    fn IOHIDEventGetFloatValue(event: IOHIDEventRef, field: u32) -> f64;
}

// Foundation must be linked for objc_getClass("NSProcessInfo") to resolve.
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const u8) -> *mut Opaque;
    fn sel_registerName(name: *const u8) -> *mut Opaque;
    fn objc_msgSend(receiver: *mut Opaque, sel: *mut Opaque, ...) -> *mut Opaque;
}

enum TempSource {
    /// Averaged CPU die sensors, degrees Celsius.
    SmcSensors,
    /// NSProcessInfo.thermalState mapped to a coarse estimate.
    ThermalPressure,
}

pub struct Reader {
    client: IOHIDEventSystemClientRef,
    source: TempSource,
}

/// Apple Silicon CPU-core die sensors are named like
/// "pACC MTR Temp Sensor4" (P-cores) / "eACC MTR Temp Sensor1" (E-cores).
fn is_cpu_die_sensor(name: &str) -> bool {
    name.contains("MTR Temp Sensor")
}

impl Reader {
    pub fn new() -> Result<Self> {
        let client = unsafe { IOHIDEventSystemClientCreate(kCFAllocatorDefault as *const c_void) };
        if !client.is_null() {
            let matching = CFDictionary::from_CFType_pairs(&[
                (
                    CFString::from_static_string("PrimaryUsagePage").as_CFType(),
                    CFNumber::from(APPLE_VENDOR_PAGE).as_CFType(),
                ),
                (
                    CFString::from_static_string("PrimaryUsage").as_CFType(),
                    CFNumber::from(TEMPERATURE_SENSOR_USAGE).as_CFType(),
                ),
            ]);
            unsafe {
                IOHIDEventSystemClientSetMatching(client, matching.as_concrete_TypeRef());
            }
            let reader = Reader {
                client,
                source: TempSource::SmcSensors,
            };
            // Only commit to the SMC source if it actually yields sensors.
            if reader.read().is_ok() {
                return Ok(reader);
            }
        }
        // Fall back to thermal pressure (public API), which always works.
        Ok(Reader {
            client: std::ptr::null_mut(),
            source: TempSource::ThermalPressure,
        })
    }

    pub fn read(&self) -> Result<f64> {
        match self.source {
            TempSource::SmcSensors => {
                let readings = self.smc_readings()?;
                let cpu: Vec<f64> = readings
                    .iter()
                    .filter(|r| is_cpu_die_sensor(&r.name))
                    .map(|r| r.celsius)
                    .collect();
                let temps: Vec<f64> = if cpu.is_empty() {
                    readings.iter().map(|r| r.celsius).collect()
                } else {
                    cpu
                };
                if temps.is_empty() {
                    bail!("no usable temperature sensors");
                }
                Ok(temps.iter().sum::<f64>() / temps.len() as f64)
            }
            TempSource::ThermalPressure => read_thermal_pressure(),
        }
    }

    pub fn source_name(&self) -> &'static str {
        match self.source {
            TempSource::SmcSensors => "SMC die sensors",
            TempSource::ThermalPressure => "thermal pressure (coarse fallback)",
        }
    }

    pub fn sensors(&self) -> Result<Vec<Reading>> {
        match self.source {
            TempSource::SmcSensors => {
                let mut readings = self.smc_readings()?;
                readings.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(readings)
            }
            TempSource::ThermalPressure => Ok(vec![Reading {
                name: "thermal pressure estimate".into(),
                celsius: read_thermal_pressure()?,
            }]),
        }
    }

    fn smc_readings(&self) -> Result<Vec<Reading>> {
        if self.client.is_null() {
            bail!("no IOHIDEventSystemClient");
        }
        let services = unsafe { IOHIDEventSystemClientCopyServices(self.client) };
        if services.is_null() {
            bail!("no SMC sensor services found");
        }
        let services: CFArray<*const c_void> =
            unsafe { CFArray::wrap_under_create_rule(services as _) };

        let product_key = CFString::from_static_string("Product");
        let mut readings = Vec::new();

        for service in services.iter() {
            let service = *service as IOHIDServiceClientRef;
            let name = unsafe {
                let prop =
                    IOHIDServiceClientCopyProperty(service, product_key.as_concrete_TypeRef());
                if prop.is_null() {
                    continue;
                }
                let s = CFString::wrap_under_create_rule(prop as _);
                s.to_string()
            };
            let event =
                unsafe { IOHIDServiceClientCopyEvent(service, EVENT_TYPE_TEMPERATURE, 0, 0) };
            if event.is_null() {
                continue;
            }
            let value = unsafe {
                let v = IOHIDEventGetFloatValue(event, TEMPERATURE_FIELD);
                CFRelease(event as CFTypeRef);
                v
            };
            if !(0.0..=125.0).contains(&value) {
                continue;
            }
            readings.push(Reading {
                name,
                celsius: value,
            });
        }

        if readings.is_empty() {
            bail!("no usable temperature sensors");
        }
        Ok(readings)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        if !self.client.is_null() {
            unsafe { CFRelease(self.client as CFTypeRef) };
        }
    }
}

// SAFETY: the client is only used from the curve loop's single thread.
unsafe impl Send for Reader {}

/// NSProcessInfo.processInfo.thermalState mapped to coarse °C estimates
/// chosen to hit sensible points on the default fan curve.
fn read_thermal_pressure() -> Result<f64> {
    let state = unsafe {
        let cls = objc_getClass(c"NSProcessInfo".to_bytes_with_nul().as_ptr());
        if cls.is_null() {
            bail!("NSProcessInfo unavailable");
        }
        let process_info = objc_msgSend(
            cls,
            sel_registerName(c"processInfo".to_bytes_with_nul().as_ptr()),
        );
        objc_msgSend(
            process_info,
            sel_registerName(c"thermalState".to_bytes_with_nul().as_ptr()),
        ) as isize
    };
    // 0 nominal, 1 fair, 2 serious, 3 critical
    Ok(match state {
        0 => 50.0,
        1 => 68.0,
        2 => 82.0,
        _ => 95.0,
    })
}
