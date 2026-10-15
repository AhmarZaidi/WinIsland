mod brightness;
mod link;
mod notification;
pub(crate) mod timer;
mod volume;

use winisland_render::{Painter, Point, Rect};

pub use self::link::LinkHit;
pub(crate) use self::link::extract_link;

use self::brightness::BrightnessMonitor;
use self::link::LinkIndicator;
use self::notification::{NotificationIndicator, NotificationMonitor};
use self::timer::TimerFinishedIndicator;
use self::volume::{VolumeIndicator, VolumeMonitor};

#[derive(Clone, Copy)]
pub struct CompactSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy)]
pub enum CompactOverlayState {
    Present,
    Defer,
    Discard,
}

pub struct CompactOverlayUpdate {
    pub volume_changed: bool,
    pub brightness_changed: bool,
    pub notification_received: bool,
}

pub struct CompactOverlay {
    volume_monitor: VolumeMonitor,
    volume_indicator: VolumeIndicator,
    brightness_monitor: BrightnessMonitor,
    brightness_indicator: VolumeIndicator,
    system_controls_overlay_enabled: bool,
    last_level_is_brightness: bool,
    notification_monitor: NotificationMonitor,
    notification_indicator: NotificationIndicator,
    link_indicator: LinkIndicator,
    timer_finished: TimerFinishedIndicator,
}

enum ActiveCompactOverlay<'a> {
    TimerFinished(&'a TimerFinishedIndicator),
    Link(&'a LinkIndicator),
    Notification(&'a NotificationIndicator),
    Volume(&'a VolumeIndicator),
    Brightness(&'a VolumeIndicator),
}

impl ActiveCompactOverlay<'_> {
    fn target_size(&self, base_width: f32, base_height: f32, scale: f32) -> CompactSize {
        match self {
            Self::TimerFinished(_) => {
                TimerFinishedIndicator::target_size(base_width, base_height, scale)
            }
            Self::Link(_) => LinkIndicator::target_size(base_width, base_height, scale),
            Self::Notification(_) => {
                NotificationIndicator::target_size(base_width, base_height, scale)
            }
            Self::Volume(_) => VolumeIndicator::target_size(base_width, base_height, scale),
            Self::Brightness(_) => VolumeIndicator::target_size(base_width, base_height, scale),
        }
    }

    fn draw(&self, painter: Painter<'_>, rect: Rect, scale: f32, alpha: f32, is_light: bool) {
        match self {
            Self::TimerFinished(indicator) => indicator.draw(painter, rect, scale, alpha, is_light),
            Self::Link(indicator) => indicator.draw(painter, rect, scale, alpha, is_light),
            Self::Notification(indicator) => indicator.draw(painter, rect, scale, alpha, is_light),
            Self::Volume(indicator) => indicator.draw(painter, rect, scale, alpha, is_light),
            Self::Brightness(indicator) => {
                indicator.draw_brightness(painter, rect, scale, alpha, is_light)
            }
        }
    }
}

impl CompactOverlay {
    pub fn new(system_controls_overlay_enabled: bool) -> Self {
        Self {
            volume_monitor: VolumeMonitor::new(system_controls_overlay_enabled),
            volume_indicator: VolumeIndicator::default(),
            brightness_monitor: BrightnessMonitor::new(),
            brightness_indicator: VolumeIndicator::new_brightness(),
            system_controls_overlay_enabled,
            last_level_is_brightness: false,
            notification_monitor: NotificationMonitor::default(),
            notification_indicator: NotificationIndicator::default(),
            link_indicator: LinkIndicator::default(),
            timer_finished: TimerFinishedIndicator::default(),
        }
    }

    pub fn show_link(&mut self, url: String) {
        self.link_indicator.show(url);
    }

    pub fn show_timer_finished(&mut self) {
        self.finish_volume_drag();
        self.finish_brightness_drag();
        self.timer_finished.show();
    }

    pub fn clear_timer_finished(&mut self) {
        self.timer_finished.clear();
    }

    pub fn update_timer_finished(&mut self) -> bool {
        self.timer_finished.update()
    }

    pub fn is_timer_finished_visible(&self) -> bool {
        self.timer_finished.is_visible()
    }

    pub fn is_timer_finished_animating(&self) -> bool {
        self.timer_finished.is_animating()
    }

    pub fn timer_finished_close_hit(&self, x: f32, y: f32, rect: Rect, scale: f32) -> bool {
        self.timer_finished.hit_close(Point::new(x, y), rect, scale)
    }

    pub fn dismiss_timer_finished(&mut self) -> bool {
        self.timer_finished.close()
    }

    pub fn is_link_prompt_active(&self) -> bool {
        matches!(self.active(), Some(ActiveCompactOverlay::Link(_)))
            && self.link_indicator.is_interactive()
    }

    pub fn link_hit(&self, x: f32, y: f32, rect: Rect, scale: f32) -> Option<LinkHit> {
        self.is_link_prompt_active()
            .then(|| self.link_indicator.hit(x, y, rect, scale))
    }

    pub fn take_link(&mut self) -> Option<String> {
        self.link_indicator.take_url()
    }

    pub fn dismiss_link(&mut self) {
        self.link_indicator.close();
    }

    pub fn set_system_controls_overlay_enabled(&mut self, enabled: bool) {
        self.system_controls_overlay_enabled = enabled;
        self.volume_monitor
            .set_native_flyout_replacement_enabled(enabled);
    }

    pub fn update(
        &mut self,
        volume_state: CompactOverlayState,
        notification_state: CompactOverlayState,
        notification_display: bool,
    ) -> CompactOverlayUpdate {
        let brightness = self.brightness_monitor.snapshot();
        let mut caps = crate::platform::capabilities();
        if brightness.available && !caps.brightness_control {
            crate::platform::update_capabilities(|caps| caps.brightness_control = true);
            caps.brightness_control = true;
        }
        let overlay_state = if self.system_controls_overlay_enabled {
            volume_state
        } else {
            CompactOverlayState::Discard
        };
        let volume_state = if caps.volume_control {
            overlay_state
        } else {
            CompactOverlayState::Discard
        };
        let notification_display = notification_display && caps.toast_events;
        self.volume_monitor.set_key_handling_enabled(
            caps.input_hooks && !matches!(volume_state, CompactOverlayState::Discard),
        );
        let volume_changed = self
            .volume_indicator
            .update(self.volume_monitor.snapshot(), volume_state)
            && self.system_controls_overlay_enabled
            && caps.volume_control;
        let brightness_state = if caps.brightness_control && brightness.available {
            overlay_state
        } else {
            CompactOverlayState::Discard
        };
        let brightness_changed = self.brightness_indicator.update_brightness(
            brightness.level,
            brightness.revision,
            brightness_state,
        ) && self.system_controls_overlay_enabled
            && caps.brightness_control
            && brightness.available;
        if volume_changed {
            self.last_level_is_brightness = false;
        }
        if brightness_changed {
            self.last_level_is_brightness = true;
        }
        let notification = self.notification_monitor.update(notification_display);
        let notification_received = if notification_display {
            self.notification_indicator
                .update(notification, notification_state)
        } else {
            self.notification_indicator.clear();
            false
        };
        CompactOverlayUpdate {
            volume_changed,
            brightness_changed,
            notification_received,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.active().is_some()
    }

    pub fn begin_volume_drag(&mut self, x: f32, y: f32, rect: Rect, scale: f32) -> bool {
        if !matches!(self.active(), Some(ActiveCompactOverlay::Volume(_)))
            || !self.volume_monitor.can_set_level()
            || !self.volume_indicator.begin_drag(x, y, rect, scale)
        {
            return false;
        }
        self.update_volume_drag(x, rect, scale);
        true
    }

    pub fn update_volume_drag(&mut self, x: f32, rect: Rect, scale: f32) {
        if let Some(level) = self.volume_indicator.drag_to(x, rect, scale) {
            self.volume_monitor.set_level(level);
        }
    }

    pub fn finish_volume_drag(&mut self) -> bool {
        self.volume_indicator.finish_drag()
    }

    pub fn is_volume_dragging(&self) -> bool {
        self.volume_indicator.is_dragging()
    }

    pub fn begin_brightness_drag(&mut self, x: f32, y: f32, rect: Rect, scale: f32) -> bool {
        if !matches!(self.active(), Some(ActiveCompactOverlay::Brightness(_)))
            || !self.brightness_monitor.snapshot().available
            || !self.brightness_indicator.begin_drag(x, y, rect, scale)
        {
            return false;
        }
        self.update_brightness_drag(x, rect, scale);
        true
    }

    pub fn update_brightness_drag(&mut self, x: f32, rect: Rect, scale: f32) {
        if let Some(level) = self.brightness_indicator.drag_to(x, rect, scale) {
            self.brightness_monitor.set_level(level);
        }
    }

    pub fn finish_brightness_drag(&mut self) -> bool {
        self.brightness_indicator.finish_drag()
    }

    pub fn is_brightness_dragging(&self) -> bool {
        self.brightness_indicator.is_dragging()
    }

    pub fn is_notification_visible(&self) -> bool {
        self.notification_indicator.is_visible()
    }

    pub fn activate_notification(&mut self) -> bool {
        let Some(notification_id) = self.notification_indicator.activate() else {
            return false;
        };
        self.notification_monitor
            .remove_notification(notification_id);
        true
    }

    pub fn dismiss_notification(&mut self) -> bool {
        let Some(notification_id) = self.notification_indicator.dismiss() else {
            return false;
        };
        self.notification_monitor
            .remove_notification(notification_id);
        true
    }

    pub fn target_size(
        &self,
        base_width: f32,
        base_height: f32,
        scale: f32,
    ) -> Option<CompactSize> {
        self.active()
            .map(|overlay| overlay.target_size(base_width, base_height, scale))
    }

    pub(crate) fn maximum_size(base_width: f32, base_height: f32, scale: f32) -> CompactSize {
        let notification = NotificationIndicator::target_size(base_width, base_height, scale);
        let volume = VolumeIndicator::target_size(base_width, base_height, scale);
        CompactSize {
            width: notification.width.max(volume.width),
            height: notification.height.max(volume.height),
        }
    }

    pub fn draw(&self, painter: Painter<'_>, rect: Rect, scale: f32, alpha: f32, is_light: bool) {
        if let Some(overlay) = self.active() {
            overlay.draw(painter, rect, scale, alpha, is_light);
        }
    }

    fn active(&self) -> Option<ActiveCompactOverlay<'_>> {
        if self.timer_finished.is_visible() {
            Some(ActiveCompactOverlay::TimerFinished(&self.timer_finished))
        } else if self.last_level_is_brightness && self.brightness_indicator.is_visible() {
            Some(ActiveCompactOverlay::Brightness(&self.brightness_indicator))
        } else if self.volume_indicator.is_visible() {
            Some(ActiveCompactOverlay::Volume(&self.volume_indicator))
        } else if self.brightness_indicator.is_visible() {
            Some(ActiveCompactOverlay::Brightness(&self.brightness_indicator))
        } else if self.link_indicator.is_visible() {
            Some(ActiveCompactOverlay::Link(&self.link_indicator))
        } else if self.notification_indicator.is_visible() {
            Some(ActiveCompactOverlay::Notification(
                &self.notification_indicator,
            ))
        } else {
            None
        }
    }
}
