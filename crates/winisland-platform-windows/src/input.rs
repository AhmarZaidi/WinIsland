use std::cell::RefCell;
use std::time::Duration;

use parking_lot::Mutex;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetDoubleClickTime, VK_LBUTTON, VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use winisland_platform::{
    BrightnessKey, BrightnessKeyEvent, InputHooks, PlatformError, VirtualKey, VolumeKey,
    VolumeKeyEvent,
};

const VK_BRIGHTNESS_DOWN: u32 = 0xD8;
const VK_BRIGHTNESS_UP: u32 = 0xD9;

type VolumeCallback = Box<dyn Fn(VolumeKeyEvent) -> bool + Send + Sync>;
type BrightnessCallback = Box<dyn Fn(BrightnessKeyEvent) -> bool + Send + Sync>;

static VOLUME_CALLBACK: Mutex<Option<VolumeCallback>> = Mutex::new(None);
static BRIGHTNESS_CALLBACK: Mutex<Option<BrightnessCallback>> = Mutex::new(None);

thread_local! {
    static KEYBOARD_HOOK: RefCell<Option<HookGuard>> = const { RefCell::new(None) };
}

struct HookGuard(HHOOK);

impl Drop for HookGuard {
    fn drop(&mut self) {
        // SAFETY: The hook was installed by this process and is removed on its message thread.
        if unsafe { UnhookWindowsHookEx(self.0) }.is_err() {
            log::warn!("Keyboard hook could not be removed");
        }
    }
}

fn ensure_keyboard_hook() -> Result<(), PlatformError> {
    KEYBOARD_HOOK.with(|slot| {
        if slot.borrow().is_none() {
            // SAFETY: The current module contains the static callback; the caller pumps messages.
            let hook = unsafe {
                let module = GetModuleHandleW(None).map_err(PlatformError::backend)?;
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard_hook),
                    Some(HINSTANCE(module.0)),
                    0,
                )
                .map_err(PlatformError::backend)?
            };
            *slot.borrow_mut() = Some(HookGuard(hook));
        }
        Ok(())
    })
}

fn release_keyboard_hook_if_idle() {
    let volume_active = VOLUME_CALLBACK.lock().is_some();
    let brightness_active = BRIGHTNESS_CALLBACK.lock().is_some();
    if !volume_active && !brightness_active {
        KEYBOARD_HOOK.with(|slot| {
            if slot.borrow_mut().take().is_some() {
                log::info!("Keyboard hook removed");
            }
        });
    }
}

pub struct WindowsInput;

impl InputHooks for WindowsInput {
    fn install_volume_keys(
        &self,
        callback: Box<dyn Fn(VolumeKeyEvent) -> bool + Send + Sync>,
    ) -> Result<(), PlatformError> {
        ensure_keyboard_hook()?;
        *VOLUME_CALLBACK.lock() = Some(callback);
        log::info!("Volume keys are handled by WinIsland");
        Ok(())
    }

    fn uninstall_volume_keys(&self) -> Result<(), PlatformError> {
        *VOLUME_CALLBACK.lock() = None;
        release_keyboard_hook_if_idle();
        log::info!("Native volume flyout replacement disabled");
        Ok(())
    }

    fn install_brightness_keys(
        &self,
        callback: Box<dyn Fn(BrightnessKeyEvent) -> bool + Send + Sync>,
    ) -> Result<(), PlatformError> {
        ensure_keyboard_hook()?;
        *BRIGHTNESS_CALLBACK.lock() = Some(callback);
        log::info!("Brightness keys are handled by WinIsland");
        Ok(())
    }

    fn uninstall_brightness_keys(&self) -> Result<(), PlatformError> {
        *BRIGHTNESS_CALLBACK.lock() = None;
        release_keyboard_hook_if_idle();
        log::info!("Native brightness flyout replacement disabled");
        Ok(())
    }

    fn key_down(&self, key: VirtualKey) -> Result<bool, PlatformError> {
        let code = match key {
            VirtualKey::LeftMouseButton => VK_LBUTTON.0 as i32,
        };
        // SAFETY: GetAsyncKeyState queries virtual key state without pointer arguments.
        Ok(unsafe { (GetAsyncKeyState(code) as u16 & 0x8000) != 0 })
    }

    fn double_click_interval(&self) -> Duration {
        // SAFETY: GetDoubleClickTime has no input pointers and reads a system setting.
        Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }))
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && matches!(
            wparam.0 as u32,
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
        )
    {
        // SAFETY: For HC_ACTION, lparam points to KBDLLHOOKSTRUCT until this callback returns.
        let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let is_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);

        let volume_key = match key.vkCode {
            value if value == VK_VOLUME_UP.0 as u32 => Some(VolumeKey::Up),
            value if value == VK_VOLUME_DOWN.0 as u32 => Some(VolumeKey::Down),
            value if value == VK_VOLUME_MUTE.0 as u32 => Some(VolumeKey::Mute),
            _ => None,
        };
        if let Some(key) = volume_key {
            let event = VolumeKeyEvent { key, is_down };
            let handled = VOLUME_CALLBACK
                .lock()
                .as_ref()
                .is_some_and(|callback| callback(event));
            if handled {
                return LRESULT(1);
            }
        }

        let brightness_key = match key.vkCode {
            VK_BRIGHTNESS_DOWN => Some(BrightnessKey::Down),
            VK_BRIGHTNESS_UP => Some(BrightnessKey::Up),
            _ => match key.scanCode {
                0x6E => Some(BrightnessKey::Down),
                0x6F => Some(BrightnessKey::Up),
                _ => None,
            },
        };
        if let Some(key) = brightness_key {
            let event = BrightnessKeyEvent { key, is_down };
            let handled = BRIGHTNESS_CALLBACK
                .lock()
                .as_ref()
                .is_some_and(|callback| callback(event));
            if handled {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: Unhandled input is forwarded with the original hook parameters.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
