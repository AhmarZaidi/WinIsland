use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
};
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::{VARIANT, VT_I4, VT_UI1, VT_UI4};
use windows::Win32::System::Wmi::{
    ISWbemEventSource, ISWbemLocator, ISWbemObject, ISWbemServices, SWbemLocator,
};
use windows::core::{BOOL, BSTR};
use winisland_platform::{BrightnessFeed, BrightnessSnapshot};

use crate::com::ComGuard;

const WMI_QUERIES: [&str; 2] = [
    "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active = TRUE",
    "SELECT CurrentBrightness FROM WmiMonitorBrightness",
];

const WMI_METHOD_QUERIES: [&str; 2] = [
    "SELECT * FROM WmiMonitorBrightnessMethods WHERE Active = TRUE",
    "SELECT * FROM WmiMonitorBrightnessMethods",
];

struct PhysicalMonitorGroup {
    monitors: Vec<PHYSICAL_MONITOR>,
}

impl Drop for PhysicalMonitorGroup {
    fn drop(&mut self) {
        if !self.monitors.is_empty() {
            // SAFETY: The monitors were allocated by GetPhysicalMonitorsFromHMONITOR and are valid.
            unsafe {
                let _ = DestroyPhysicalMonitors(&self.monitors);
            }
        }
    }
}

pub(super) struct WindowsBrightnessFeed {
    snapshot: Arc<Mutex<BrightnessSnapshot>>,
    sender: SyncSender<f32>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl WindowsBrightnessFeed {
    pub(super) fn new() -> Self {
        let snapshot = Arc::new(Mutex::new(BrightnessSnapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(8);
        let worker_snapshot = snapshot.clone();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let Ok(_com) = ComGuard::mta() else {
                log::debug!("Windows brightness monitor could not initialize COM");
                return;
            };
            if let Err(error) = run_monitor(&worker_snapshot, &receiver, &worker_stop) {
                log::debug!("Windows brightness monitor finished: {error}");
            }
        });
        Self {
            snapshot,
            sender,
            stop,
            worker: Some(worker),
        }
    }
}

impl BrightnessFeed for WindowsBrightnessFeed {
    fn snapshot(&self) -> BrightnessSnapshot {
        *self.snapshot.lock()
    }

    fn set_level(&self, level: f32) {
        if level.is_finite() {
            let level = level.clamp(0.0, 1.0);
            let mut current = self.snapshot.lock();
            if current.available {
                let changed = (current.level - level).abs() > 0.001;
                current.level = level;
                if changed {
                    current.revision = current.revision.wrapping_add(1);
                }
                drop(current);
                if changed {
                    hide_native_osd();
                    crate::window::wake();
                }
                let _ = self.sender.try_send(level);
            }
        }
    }

    fn step(&self, up: bool) {
        let mut current = self.snapshot.lock();
        if !current.available {
            return;
        }
        let step = 0.10;
        let target = if up {
            ((current.level / step).round() + 1.0) * step
        } else {
            ((current.level / step).round() - 1.0) * step
        };
        let target = (target.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        current.level = target;
        current.revision = current.revision.wrapping_add(1);
        drop(current);
        hide_native_osd();
        crate::window::wake();
        let _ = self.sender.try_send(target);
    }
}

impl Drop for WindowsBrightnessFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn available() -> bool {
    let Ok(_com) = ComGuard::mta() else {
        return false;
    };
    let wmi_available =
        unsafe { connect_wmi().is_ok_and(|services| read_wmi_brightness(&services).is_some()) };
    if wmi_available {
        return true;
    }
    let ddc_groups = get_physical_monitor_groups();
    read_ddc_brightness(&ddc_groups).is_some()
}

fn run_monitor(
    snapshot: &Mutex<BrightnessSnapshot>,
    receiver: &mpsc::Receiver<f32>,
    stop: &AtomicBool,
) -> windows::core::Result<()> {
    // SAFETY: COM is initialized on this worker thread.
    let wmi_services = unsafe { connect_wmi().ok() };
    let ddc_groups = get_physical_monitor_groups();

    let initial_level = wmi_services
        .as_ref()
        .and_then(read_wmi_brightness)
        .or_else(|| read_ddc_brightness(&ddc_groups));

    let Some(mut current_level) = initial_level else {
        return Ok(());
    };

    publish(snapshot, current_level, false);

    let wmi_events: Option<ISWbemEventSource> = wmi_services.as_ref().and_then(|svc| {
        // SAFETY: The query is executed on the COM thread.
        unsafe {
            svc.ExecNotificationQuery(
                &BSTR::from("SELECT * FROM WmiMonitorBrightnessEvent WHERE Active = TRUE"),
                &BSTR::from("WQL"),
                0,
                None,
            )
            .ok()
        }
    });

    let mut last_poll = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let mut requested_level: Option<f32> = None;
        while let Ok(level) = receiver.try_recv() {
            requested_level = Some(level);
        }

        if let Some(level) = requested_level {
            if let Some(ref svc) = wmi_services {
                let u8_level = (level * 100.0).round().clamp(0.0, 100.0) as u8;
                let _ = set_wmi_brightness(svc, u8_level);
            }
            let _ = set_ddc_brightness(&ddc_groups, level);
            current_level = level;
        }

        if let Some(ref events) = wmi_events {
            // SAFETY: Polling WMI event on COM thread with short timeout.
            if let Ok(event) = unsafe { events.NextEvent(30) }
                && let Ok(value) = unsafe { property_u32(&event, "Brightness") }
            {
                let lvl = (value.min(100) as f32) / 100.0;
                if (lvl - current_level).abs() > 0.005 {
                    current_level = lvl;
                    publish(snapshot, lvl, true);
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(30));
        }

        if last_poll.elapsed() >= Duration::from_millis(150) {
            last_poll = Instant::now();
            let polled = wmi_services
                .as_ref()
                .and_then(read_wmi_brightness)
                .or_else(|| read_ddc_brightness(&ddc_groups));

            if let Some(lvl) = polled
                && (lvl - current_level).abs() > 0.005
            {
                current_level = lvl;
                publish(snapshot, lvl, true);
            }
        }
    }

    Ok(())
}

unsafe extern "system" fn collect_monitors_cb(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    // SAFETY: lparam is a valid pointer to Vec<PhysicalMonitorGroup> on this call stack.
    let groups = unsafe { &mut *(lparam.0 as *mut Vec<PhysicalMonitorGroup>) };
    let mut count = 0u32;
    // SAFETY: hmonitor is a valid display monitor handle provided by EnumDisplayMonitors.
    if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmonitor, &mut count) }.is_ok() && count > 0
    {
        let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
        // SAFETY: monitors has length count and is writable.
        if unsafe { GetPhysicalMonitorsFromHMONITOR(hmonitor, &mut monitors) }.is_ok() {
            groups.push(PhysicalMonitorGroup { monitors });
        }
    }
    BOOL(1)
}

fn get_physical_monitor_groups() -> Vec<PhysicalMonitorGroup> {
    let mut groups = Vec::new();
    // SAFETY: Callback receives a pointer to local groups vector for the duration of the call.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitors_cb),
            LPARAM(&mut groups as *mut _ as isize),
        );
    }
    groups
}

fn read_ddc_brightness(groups: &[PhysicalMonitorGroup]) -> Option<f32> {
    for group in groups {
        for mon in &group.monitors {
            let mut min = 0u32;
            let mut cur = 0u32;
            let mut max = 0u32;
            // SAFETY: hPhysicalMonitor was returned by GetPhysicalMonitorsFromHMONITOR and is valid.
            let ok =
                unsafe { GetMonitorBrightness(mon.hPhysicalMonitor, &mut min, &mut cur, &mut max) };
            if ok != 0 && max > min {
                let level = (cur.saturating_sub(min) as f32) / ((max - min) as f32);
                return Some(level.clamp(0.0, 1.0));
            }
        }
    }
    None
}

fn set_ddc_brightness(groups: &[PhysicalMonitorGroup], level: f32) -> bool {
    let mut any_success = false;
    for group in groups {
        for mon in &group.monitors {
            let mut min = 0u32;
            let mut cur = 0u32;
            let mut max = 0u32;
            // SAFETY: hPhysicalMonitor is valid and owned by the caller's group.
            let ok =
                unsafe { GetMonitorBrightness(mon.hPhysicalMonitor, &mut min, &mut cur, &mut max) };
            if ok != 0 && max > min {
                let target = min + ((level * (max - min) as f32).round() as u32).min(max - min);
                // SAFETY: Setting brightness on a valid physical monitor handle.
                let set_ok = unsafe { SetMonitorBrightness(mon.hPhysicalMonitor, target) };
                if set_ok != 0 {
                    any_success = true;
                }
            }
        }
    }
    any_success
}

fn read_wmi_brightness(services: &ISWbemServices) -> Option<f32> {
    for query in &WMI_QUERIES {
        // SAFETY: Caller keeps the WMI service on its COM-initialized thread.
        let objects = unsafe {
            services
                .ExecQuery(&BSTR::from(*query), &BSTR::from("WQL"), 0, None)
                .ok()
        };
        if let Some(objects) = objects {
            let count = unsafe { objects.Count() }.unwrap_or(0);
            for i in 0..count {
                if let Ok(obj) = unsafe { objects.ItemIndex(i) }
                    && let Ok(val) = unsafe { property_u32(&obj, "CurrentBrightness") }
                {
                    return Some((val.min(100) as f32) / 100.0);
                }
            }
        }
    }
    None
}

fn set_wmi_brightness(services: &ISWbemServices, level: u8) -> bool {
    let mut any_success = false;
    for query in &WMI_METHOD_QUERIES {
        // SAFETY: Caller keeps the WMI service on its COM-initialized thread.
        let objects = unsafe {
            services
                .ExecQuery(&BSTR::from(*query), &BSTR::from("WQL"), 0, None)
                .ok()
        };
        if let Some(objects) = objects {
            let count = unsafe { objects.Count() }.unwrap_or(0);
            for i in 0..count {
                if let Ok(obj) = unsafe { objects.ItemIndex(i) } {
                    // SAFETY: Method execution stays on the COM thread.
                    if unsafe { set_brightness(&obj, level) }.is_ok() {
                        any_success = true;
                    }
                }
            }
        }
        if any_success {
            break;
        }
    }
    any_success
}

unsafe fn connect_wmi() -> windows::core::Result<ISWbemServices> {
    // SAFETY: Caller runs on a COM-initialized thread and keeps the service on it.
    unsafe {
        let locator: ISWbemLocator = CoCreateInstance(&SWbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let empty = BSTR::new();
        locator.ConnectServer(
            &empty,
            &BSTR::from("ROOT\\WMI"),
            &empty,
            &empty,
            &empty,
            &empty,
            0,
            None,
        )
    }
}

unsafe fn property_u32(object: &ISWbemObject, name: &str) -> windows::core::Result<u32> {
    // SAFETY: The property is read from a live object on its COM thread.
    let value = unsafe { object.Properties_()?.Item(&BSTR::from(name), 0)?.Value()? };
    // SAFETY: The variant members are read only after checking their discriminant.
    let inner = unsafe { &value.Anonymous.Anonymous };
    let number = match inner.vt {
        VT_UI1 => unsafe { inner.Anonymous.bVal as u32 },
        VT_UI4 => unsafe { inner.Anonymous.ulVal },
        VT_I4 => unsafe { inner.Anonymous.lVal.max(0) as u32 },
        _ => 0,
    };
    Ok(number)
}

unsafe fn set_brightness(object: &ISWbemObject, level: u8) -> windows::core::Result<()> {
    // SAFETY: The WMI method and input object remain on their creating COM thread.
    unsafe {
        let method = object
            .Methods_()?
            .Item(&BSTR::from("WmiSetBrightness"), 0)?;
        let input = method.InParameters()?.SpawnInstance_(0)?;
        let mut brightness = VARIANT::default();
        (*brightness.Anonymous.Anonymous).vt = VT_UI1;
        (*brightness.Anonymous.Anonymous).Anonymous.bVal = level;
        input
            .Properties_()?
            .Item(&BSTR::from("Brightness"), 0)?
            .SetValue(&brightness)?;
        let mut timeout = VARIANT::default();
        (*timeout.Anonymous.Anonymous).vt = VT_UI4;
        (*timeout.Anonymous.Anonymous).Anonymous.ulVal = 0;
        input
            .Properties_()?
            .Item(&BSTR::from("Timeout"), 0)?
            .SetValue(&timeout)?;
        object.ExecMethod_(&BSTR::from("WmiSetBrightness"), &input, 0, None)?;
    }
    Ok(())
}

fn hide_native_osd() {
    // SAFETY: Interacting with window handles via Win32 APIs to hide native OSD flyout.
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, SW_HIDE, ShowWindow};
        use windows::core::w;

        let mut hwnd = FindWindowExW(None, None, w!("NativeHWNDHost"), None).unwrap_or_default();
        while !hwnd.is_invalid() && !hwnd.0.is_null() {
            let _ = ShowWindow(hwnd, SW_HIDE);
            hwnd = FindWindowExW(None, Some(hwnd), w!("NativeHWNDHost"), None).unwrap_or_default();
        }
    }
}

fn publish(snapshot: &Mutex<BrightnessSnapshot>, level: f32, notify: bool) {
    let mut current = snapshot.lock();
    let changed = (current.level - level).abs() > 0.001;
    if changed && notify {
        current.revision = current.revision.wrapping_add(1);
    }
    current.level = level;
    current.available = true;
    drop(current);
    if changed && notify {
        hide_native_osd();
        crate::window::wake();
    }
}
