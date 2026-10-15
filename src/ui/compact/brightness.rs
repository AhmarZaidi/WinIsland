use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use winisland_platform::{BrightnessFeed, BrightnessKey, BrightnessKeyEvent, BrightnessSnapshot};

pub(super) struct BrightnessMonitor {
    feed: Option<Arc<dyn BrightnessFeed>>,
    display_enabled: Arc<AtomicBool>,
    keyboard_hook_installed: bool,
}

impl BrightnessMonitor {
    pub(super) fn new(replace_native_brightness_flyout: bool) -> Self {
        let feed = match crate::platform::display().start_brightness_feed() {
            Ok(feed) => Some(Arc::from(feed)),
            Err(error) => {
                log::warn!("Brightness monitor unavailable: {error}");
                None
            }
        };
        let mut monitor = Self {
            feed,
            display_enabled: Arc::new(AtomicBool::new(false)),
            keyboard_hook_installed: false,
        };
        monitor.set_native_flyout_replacement_enabled(replace_native_brightness_flyout);
        monitor
    }

    pub(super) fn snapshot(&self) -> BrightnessSnapshot {
        self.feed
            .as_ref()
            .map(|feed| feed.snapshot())
            .unwrap_or_default()
    }

    pub(super) fn set_level(&self, level: f32) {
        if let Some(feed) = &self.feed {
            feed.set_level(level);
        }
    }

    pub(super) fn set_key_handling_enabled(&self, enabled: bool) {
        self.display_enabled.store(enabled, Ordering::Release);
    }

    pub(super) fn set_native_flyout_replacement_enabled(&mut self, enabled: bool) {
        if enabled && crate::platform::capabilities().input_hooks {
            if !self.keyboard_hook_installed
                && let Some(feed) = self.feed.clone()
            {
                let display_enabled = self.display_enabled.clone();
                let callback = Box::new(move |event: BrightnessKeyEvent| {
                    let ready = display_enabled.load(Ordering::Acquire);
                    if ready && event.is_down {
                        match event.key {
                            BrightnessKey::Up => feed.step(true),
                            BrightnessKey::Down => feed.step(false),
                        }
                    }
                    ready
                });
                match crate::platform::input().install_brightness_keys(callback) {
                    Ok(()) => self.keyboard_hook_installed = true,
                    Err(error) => {
                        log::warn!("Brightness keyboard hook could not be installed: {error}");
                    }
                }
            }
        } else {
            self.remove_keyboard_hook();
        }
    }

    fn remove_keyboard_hook(&mut self) {
        self.display_enabled.store(false, Ordering::Release);
        if self.keyboard_hook_installed {
            if let Err(error) = crate::platform::input().uninstall_brightness_keys() {
                log::warn!("Brightness keyboard hook could not be removed: {error}");
            }
            self.keyboard_hook_installed = false;
        }
    }
}

impl Drop for BrightnessMonitor {
    fn drop(&mut self) {
        self.remove_keyboard_hook();
    }
}
