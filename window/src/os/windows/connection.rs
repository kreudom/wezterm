//! The connection to the GUI subsystem
use super::{HWindow, WindowInner};
use crate::connection::ConnectionOps;
use crate::screen::{ScreenInfo, Screens};
use crate::spawn::*;
use crate::{Appearance, ScreenRect};
use anyhow::Context;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::ptr::null_mut;
use std::rc::Rc;
use winapi::shared::minwindef::*;
use winapi::shared::windef::*;
use winapi::shared::winerror::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use winapi::um::winbase::INFINITE;
use winapi::um::wingdi::{DISPLAY_DEVICEW, QDC_ONLY_ACTIVE_PATHS, QDC_VIRTUAL_MODE_AWARE};
use winapi::um::winnt::HANDLE;
use winapi::um::winuser::*;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME,
};
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

pub struct Connection {
    event_handle: HANDLE,
    pub(crate) windows: RefCell<HashMap<HWindow, Rc<RefCell<WindowInner>>>>,
    pub(crate) gl_connection: RefCell<Option<Rc<crate::egl::GlConnection>>>,
}

pub(crate) fn get_appearance() -> Appearance {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize") {
        Ok(theme) => {
            let light = theme.get_value::<u32, _>("AppsUseLightTheme").unwrap_or(1) == 1;
            if light {
                Appearance::Light
            } else {
                Appearance::Dark
            }
        }
        _ => Appearance::Light,
    }
}

impl ConnectionOps for Connection {
    fn terminate_message_loop(&self) {
        unsafe {
            PostQuitMessage(0);
        }
    }

    fn get_appearance(&self) -> Appearance {
        get_appearance()
    }

    fn run_message_loop(&self) -> anyhow::Result<()> {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        loop {
            SPAWN_QUEUE.run();

            let res = unsafe { PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) };
            if res != 0 {
                if msg.message == WM_QUIT {
                    // Clear our state before we exit, otherwise we can
                    // trigger `drop` handlers during shutdown and that
                    // can have bad interactions
                    self.windows.borrow_mut().clear();
                    return Ok(());
                }

                unsafe {
                    // We don't want to call TranslateMessage here
                    // unconditionally.  Instead, we perform translation
                    // in a handful of special cases in window.rs.
                    DispatchMessageW(&mut msg);
                }
            } else {
                self.wait_message();
            }
        }
    }

    fn beep(&self) {
        unsafe {
            MessageBeep(MB_OK);
        }
    }

    fn screens(&self) -> anyhow::Result<Screens> {
        struct Info {
            primary: Option<ScreenInfo>,
            active: Option<ScreenInfo>,
            by_name: HashMap<String, ScreenInfo>,
            virtual_rect: ScreenRect,
            active_handle: HMONITOR,
            friendly_names: HashMap<String, String>,
            gdi_to_adapater: HashMap<String, String>,
        }

        unsafe extern "system" fn callback(
            mon: HMONITOR,
            _hdc: HDC,
            _rect: *mut RECT,
            data: LPARAM,
        ) -> i32 {
            let info: &mut Info = &mut *(data as *mut Info);
            let mut mi: MONITORINFOEXW = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            GetMonitorInfoW(mon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO);

            let monitor_name = wstr(&mi.szDevice);
            let friendly_name = match info.friendly_names.get(&monitor_name) {
                Some(name) => name.to_string(),
                None => {
                    // Fall back to EnumDisplayDevicesW.
                    // It likely has a terribly generic name like "Generic PnP Monitor".
                    let mut display_device: DISPLAY_DEVICEW = std::mem::zeroed();
                    display_device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;

                    if EnumDisplayDevicesW(mi.szDevice.as_ptr(), 0, &mut display_device, 0) != 0 {
                        wstr(&display_device.DeviceString)
                    } else {
                        "Unknown".to_string()
                    }
                }
            };

            let adapter_name = match info.gdi_to_adapater.get(&monitor_name) {
                Some(name) => name.to_string(),
                None => "Unknown".to_string(),
            };

            // "\\.\DISPLAY1" -> "DISPLAY1"
            let monitor_name = if let Some(name) = monitor_name.strip_prefix("\\\\.\\") {
                name.to_string()
            } else {
                monitor_name
            };

            let monitor_name = format!("{monitor_name}: {friendly_name} on {adapter_name}");

            let screen_info = ScreenInfo {
                name: monitor_name.clone(),
                rect: euclid::rect(
                    mi.rcMonitor.left as isize,
                    mi.rcMonitor.top as isize,
                    mi.rcMonitor.right as isize - mi.rcMonitor.left as isize,
                    mi.rcMonitor.bottom as isize - mi.rcMonitor.top as isize,
                ),
                scale: 1.0,
            };

            info.virtual_rect = info.virtual_rect.union(&screen_info.rect);

            if mi.dwFlags & MONITORINFOF_PRIMARY == MONITORINFOF_PRIMARY {
                info.primary.replace(screen_info.clone());
            }
            if mon == info.active_handle {
                info.active.replace(screen_info.clone());
            }

            info.by_name.insert(monitor_name, screen_info);

            winapi::shared::ntdef::TRUE.into()
        }

        let mut info = Info {
            primary: None,
            active: None,
            by_name: HashMap::new(),
            virtual_rect: euclid::rect(0, 0, 0, 0),
            active_handle: unsafe { MonitorFromWindow(GetFocus(), MONITOR_DEFAULTTONEAREST) },
            friendly_names: gdi_display_name_to_friendly_monitor_names()?,
            gdi_to_adapater: gdi_display_name_to_adapter_names(),
        };
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(callback),
                &mut info as *mut _ as LPARAM,
            );
        }

        let main = info
            .primary
            .ok_or_else(|| anyhow::anyhow!("There is no primary monitor configured!?"))?;
        let active = info.active.unwrap_or_else(|| main.clone());

        Ok(Screens {
            main,
            active,
            by_name: info.by_name,
            virtual_rect: info.virtual_rect,
        })
    }
}

impl Connection {
    pub(crate) fn create_new() -> anyhow::Result<Self> {
        let event_handle = SPAWN_QUEUE.event_handle.0;
        Ok(Self {
            event_handle,
            windows: RefCell::new(HashMap::new()),
            gl_connection: RefCell::new(None),
        })
    }

    fn wait_message(&self) {
        unsafe {
            MsgWaitForMultipleObjects(
                1,
                &self.event_handle,
                0,
                INFINITE,
                QS_ALLEVENTS | QS_ALLINPUT | QS_ALLPOSTMESSAGE,
            );
        }
    }

    pub(crate) fn get_window(&self, handle: HWindow) -> Option<Rc<RefCell<WindowInner>>> {
        self.windows.borrow().get(&handle).map(Rc::clone)
    }

    pub(crate) fn with_window_inner<
        R,
        F: FnOnce(&mut WindowInner) -> anyhow::Result<R> + Send + 'static,
    >(
        window: HWindow,
        f: F,
    ) -> promise::Future<R>
    where
        R: Send + 'static,
    {
        let mut prom = promise::Promise::new();
        let future = prom.get_future().unwrap();
        promise::spawn::spawn_into_main_thread(async move {
            if let Some(handle) = Connection::get()
                .expect("Connection::init has not been called")
                .get_window(window)
            {
                let mut inner = handle.borrow_mut();
                prom.result(f(&mut inner));
            }
        })
        .detach();

        future
    }
}

/// Convert a UCS2 wide char string to a Rust String
fn wstr(slice: &[u16]) -> String {
    let len = slice.iter().position(|&c| c == 0).unwrap_or(0);
    OsString::from_wide(&slice[0..len])
        .to_string_lossy()
        .to_string()
}

/// Build a mapping of GDI paths like `\\.\DISPLAY6` to the name of the associated
/// display adapter eg: `NVIDIA GeForce RTX 3080 Ti`.
fn gdi_display_name_to_adapter_names() -> HashMap<String, String> {
    let mut map = HashMap::new();

    let mut display_device: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
    display_device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;

    for n in 0.. {
        if unsafe { EnumDisplayDevicesW(std::ptr::null(), n, &mut display_device, 0) } == 0 {
            break;
        }
        let adapter_name = wstr(&display_device.DeviceString);
        let gdi_name = wstr(&display_device.DeviceName);

        map.insert(gdi_name, adapter_name);
    }
    map
}

/// Build a mapping of GDI paths like `\\.\DISPLAY6` to the corresponding friendly name of
/// the associated monitor eg: `Gigabyte M32U`.
fn gdi_display_name_to_friendly_monitor_names() -> anyhow::Result<HashMap<String, String>> {
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![];
    let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![];
    let mut map = HashMap::new();

    let flags = QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE;

    loop {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;

        let result = unsafe {
            GetDisplayConfigBufferSizes(flags, &mut path_count as *mut _, &mut mode_count as *mut _)
        };

        if result != ERROR_SUCCESS as _ {
            return Err(std::io::Error::last_os_error()).context("GetDisplayConfigBufferSizes");
        }

        unsafe {
            paths.resize_with(path_count as usize, || std::mem::zeroed());
            modes.resize_with(mode_count as usize, || std::mem::zeroed());
        }

        let result = unsafe {
            QueryDisplayConfig(
                flags,
                &mut path_count as *mut _,
                paths.as_mut_ptr(),
                &mut mode_count as &mut _,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };

        // Shrink down if fewer paths than were requested were
        // returned to us
        unsafe {
            paths.resize_with(path_count as usize, || std::mem::zeroed());
            modes.resize_with(mode_count as usize, || std::mem::zeroed());
        }

        if result == ERROR_INSUFFICIENT_BUFFER as _ {
            continue;
        }

        if result != ERROR_SUCCESS as _ {
            return Err(std::io::Error::last_os_error()).context("QueryDisplayConfig");
        }

        break;
    }

    for path in &paths {
        let mut target_name: DISPLAYCONFIG_TARGET_DEVICE_NAME = unsafe { std::mem::zeroed() };

        target_name.header.adapterId = path.targetInfo.adapterId;
        target_name.header.id = path.targetInfo.id;
        target_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target_name.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;

        let result = unsafe { DisplayConfigGetDeviceInfo(&mut target_name.header) };
        if result != ERROR_SUCCESS as _ {
            return Err(std::io::Error::last_os_error())
                .context("DisplayConfigGetDeviceInfo DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME");
        }

        let mut source_name: DISPLAYCONFIG_SOURCE_DEVICE_NAME = unsafe { std::mem::zeroed() };
        source_name.header.adapterId = path.targetInfo.adapterId;
        source_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source_name.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;

        let result = unsafe { DisplayConfigGetDeviceInfo(&mut source_name.header) };
        if result != ERROR_SUCCESS as _ {
            return Err(std::io::Error::last_os_error())
                .context("DisplayConfigGetDeviceInfo DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME");
        }

        let name = wstr(&target_name.monitorFriendlyDeviceName);
        let gdi_name = wstr(&source_name.viewGdiDeviceName);

        map.insert(gdi_name, name);
    }
    Ok(map)
}
