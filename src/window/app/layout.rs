use std::time::Duration;

use crate::platform::{MonitorRef, WindowRef, window};
use winisland_core::config::{
    DockPosition, MAX_HIDDEN_WIDTH, MAX_LYRIC_WIDTH, PADDING, TOP_OFFSET,
};
use winisland_platform::{OverlayStyles, WindowPosition, WindowSize};
use winisland_render::text::FontManager;
use winisland_render::{Path, Rect};

use super::{App, DEFAULT_ANIMATION_REFRESH_RATE_MILLIHERTZ, IslandLayout};

impl App {
    pub(super) fn required_window_size(&self) -> WindowSize {
        let compact_scale = self.config.compact_scale;
        let expanded_scale = self.config.expanded_scale;
        let activity_size = self.activity_mgr.preferred_size();
        let compact_width = crate::ui::widget::compact::target_width(
            &self.config.compact_widget_layout,
            self.config.base_width,
            Some(MAX_LYRIC_WIDTH.max(activity_size.0)),
        ) * compact_scale;
        let plugin_widths = crate::ui::plugin::compact_widths(self.plugin_host.as_deref());
        let compact_width = compact_width + (plugin_widths.0 + plugin_widths.1) * compact_scale;
        let compact_overlay = crate::ui::compact::CompactOverlay::maximum_size(
            self.config.base_width,
            self.config.base_height,
            compact_scale,
        );
        let compact_height = self.config.base_height.max(activity_size.1) * compact_scale;
        let compact_lyric_height = if self.config.show_secondary_lyrics {
            compact_height.max(crate::ui::island::mini_lyric_pair_height(
                self.config.font_size,
                compact_scale,
            ))
        } else {
            compact_height
        };
        let multitask_extent = if self.config.device_status_indicators {
            crate::ui::island::multitask::side_extent(compact_scale, compact_lyric_height)
        } else {
            0.0
        };
        let width = compact_width
            .max(compact_overlay.width)
            .max(self.config.expanded_width * expanded_scale)
            + 2.0 * multitask_extent;
        let height = compact_lyric_height.max(compact_overlay.height).max(
            (self.config.expanded_height + crate::ui::expanded::pager::PAGER_EXTENT)
                * expanded_scale,
        );
        WindowSize::new((width + PADDING) as u32, (height + PADDING) as u32)
    }

    pub(super) fn get_target_monitor(window: &WindowRef, monitor_index: i32) -> Option<MonitorRef> {
        window.target_monitor(monitor_index)
    }
    pub(super) fn update_display_frame_interval(&mut self, monitor: &MonitorRef) {
        let refresh_rate_millihertz = monitor
            .refresh_rate_millihertz()
            .filter(|refresh_rate| *refresh_rate > 0)
            .unwrap_or(DEFAULT_ANIMATION_REFRESH_RATE_MILLIHERTZ);
        self.display_frame_interval =
            Duration::from_nanos(1_000_000_000_000u64 / u64::from(refresh_rate_millihertz));
    }

    pub(super) fn enforce_overlay_window(window_ref: &WindowRef) {
        window().apply_overlay_styles(
            window_ref.id(),
            OverlayStyles {
                skip_taskbar: true,
                topmost: true,
            },
        );
    }

    pub(super) fn set_configured_window_position(
        &mut self,
        window: &WindowRef,
        position_x: i32,
        position_y: i32,
    ) {
        self.geom.configured_x = position_x;
        self.geom.configured_y = position_y;
        self.geom.win_x = position_x;
        if self.hide.origin.is_some() {
            self.hide.origin = Some((position_x, position_y));
            self.snap_to_top_edge(window);
            return;
        }
        self.geom.win_y = position_y;
        window.set_outer_position(WindowPosition::new(position_x, position_y));
    }

    pub(super) fn compute_window_position(
        &self,
        mon_pos: WindowPosition,
        mon_size: WindowSize,
    ) -> (i32, i32) {
        let window_size = self.required_window_size();
        let dock_position = self.automatic_dock_position(mon_pos, mon_size);
        let (collapsed_center_x, collapsed_center_y) =
            self.collapsed_island_center(mon_pos, mon_size);
        let compact_scale = self.config.compact_scale as f64;
        let base_half_w = self.config.base_width as f64 * compact_scale / 2.0;
        let base_half_h = self.config.base_height as f64 * compact_scale / 2.0;

        let (anchor_x, local_anchor_x) = if dock_position.is_left() {
            (collapsed_center_x - base_half_w, TOP_OFFSET as f64)
        } else if dock_position.is_right() {
            (
                collapsed_center_x + base_half_w,
                window_size.width as f64 - TOP_OFFSET as f64,
            )
        } else {
            (collapsed_center_x, window_size.width as f64 / 2.0)
        };
        let (anchor_y, local_anchor_y) = if self.config.notch_mode {
            (mon_pos.y as f64, 0.0)
        } else if dock_position.is_bottom() {
            (
                collapsed_center_y + base_half_h,
                window_size.height as f64 - TOP_OFFSET as f64,
            )
        } else {
            (collapsed_center_y - base_half_h, TOP_OFFSET as f64)
        };

        (
            (anchor_x - local_anchor_x).round() as i32,
            (anchor_y - local_anchor_y).round() as i32,
        )
    }

    fn collapsed_island_center(&self, mon_pos: WindowPosition, mon_size: WindowSize) -> (f64, f64) {
        let scale = self.config.compact_scale as f64;
        let top_offset = if self.config.notch_mode {
            0.0
        } else {
            TOP_OFFSET as f64
        };
        let y_offset = if self.config.notch_mode {
            0.0
        } else {
            self.config.position_y_offset as f64
        };
        (
            mon_pos.x as f64 + mon_size.width as f64 / 2.0 + self.config.position_x_offset as f64,
            mon_pos.y as f64 + top_offset + self.config.base_height as f64 * scale / 2.0 + y_offset,
        )
    }

    fn automatic_dock_position(
        &self,
        mon_pos: WindowPosition,
        mon_size: WindowSize,
    ) -> DockPosition {
        let (center_x, center_y) = self.collapsed_island_center(mon_pos, mon_size);
        let compact_scale = self.config.compact_scale as f64;
        let expanded_scale = self.config.expanded_scale as f64;
        let base_half_h = self.config.base_height as f64 * compact_scale / 2.0;
        let expanded_half_w = self.config.expanded_width as f64 * expanded_scale / 2.0;
        let expanded_h = self.config.expanded_height as f64 * expanded_scale;
        let horizontal = if center_x - expanded_half_w <= mon_pos.x as f64 {
            -1
        } else if center_x + expanded_half_w >= (mon_pos.x as f64 + f64::from(mon_size.width)) {
            1
        } else {
            0
        };
        let bottom =
            center_y - base_half_h + expanded_h >= (mon_pos.y as f64 + f64::from(mon_size.height));

        match (bottom, horizontal) {
            (false, -1) => DockPosition::TopLeft,
            (false, 1) => DockPosition::TopRight,
            (false, _) => DockPosition::TopCenter,
            (true, -1) => DockPosition::BottomLeft,
            (true, 1) => DockPosition::BottomRight,
            (true, _) => DockPosition::BottomCenter,
        }
    }

    pub(super) fn migrate_legacy_dock_position(
        &mut self,
        mon_pos: WindowPosition,
        mon_size: WindowSize,
    ) -> bool {
        let Some(dock_position) = self.config.legacy_dock_position.take() else {
            return false;
        };
        let scale = self.config.compact_scale as f64;
        let base_half_w = self.config.base_width as f64 * scale / 2.0;
        let base_half_h = self.config.base_height as f64 * scale / 2.0;
        let center_x = if dock_position.is_left() {
            mon_pos.x as f64
                + TOP_OFFSET as f64
                + self.config.position_x_offset as f64
                + base_half_w
        } else if dock_position.is_right() {
            mon_pos.x as f64 + mon_size.width as f64 - TOP_OFFSET as f64
                + self.config.position_x_offset as f64
                - base_half_w
        } else {
            mon_pos.x as f64 + mon_size.width as f64 / 2.0 + self.config.position_x_offset as f64
        };
        let center_y = if dock_position.is_bottom() {
            mon_pos.y as f64 + mon_size.height as f64 - TOP_OFFSET as f64
                + self.config.position_y_offset as f64
                - base_half_h
        } else {
            mon_pos.y as f64
                + TOP_OFFSET as f64
                + self.config.position_y_offset as f64
                + base_half_h
        };

        self.config.position_x_offset =
            (center_x - mon_pos.x as f64 - mon_size.width as f64 / 2.0).round() as i32;
        self.config.position_y_offset =
            (center_y - mon_pos.y as f64 - TOP_OFFSET as f64 - base_half_h).round() as i32;
        crate::core::persistence::save_config(&self.config);
        log::info!("Migrated legacy dock position to automatic placement");
        true
    }

    fn island_monitor(&self, window: &WindowRef) -> Option<MonitorRef> {
        let target = Self::get_target_monitor(window, self.config.monitor_index)?;
        let (center_x, center_y) = self.collapsed_island_center(target.position(), target.size());
        let (x, y) = (center_x.round() as i32, center_y.round() as i32);
        crate::platform::window()
            .monitors()
            .into_iter()
            .find(|monitor| {
                let bounds = monitor.bounds;
                (bounds.left..bounds.right).contains(&x) && (bounds.top..bounds.bottom).contains(&y)
            })
            .map(MonitorRef)
            .or(Some(target))
    }

    pub(super) fn snap_to_top_edge(&mut self, window: &WindowRef) {
        let Some(monitor) = self.island_monitor(window) else {
            return;
        };
        let mon_pos = monitor.position();
        self.geom.win_y = mon_pos.y;
        window.set_outer_position(WindowPosition::new(self.geom.win_x, self.geom.win_y));
    }

    pub(super) fn restore_hide_origin(&mut self, window: &WindowRef) {
        if self.hide.has_hidden_reason() || self.springs.hide.value > 0.001 {
            return;
        }
        if let Some((win_x, win_y)) = self.hide.origin.take() {
            self.geom.win_x = win_x;
            self.geom.win_y = if self.config.notch_mode {
                self.island_monitor(window)
                    .map_or(win_y, |m| m.position().y)
            } else {
                win_y
            };
            window.set_outer_position(WindowPosition::new(self.geom.win_x, self.geom.win_y));
        }
    }

    fn hidden_visible_height(&self) -> f64 {
        let edge_size = self.springs.h.value as f64;
        if self.config.hidden_width >= MAX_HIDDEN_WIDTH && !self.fullscreen_hide_active() {
            edge_size
        } else {
            let hidden_width = if self.fullscreen_hide_active() {
                winisland_core::config::MIN_HIDDEN_WIDTH
            } else {
                self.config.hidden_width
            };
            let configured = hidden_width as f64 * self.config.compact_scale as f64;
            if configured <= f64::EPSILON {
                1.0_f64.min(edge_size)
            } else {
                configured.min(edge_size)
            }
        }
    }

    pub(super) fn can_hide(&self) -> bool {
        let edge_size = self.springs.h.value as f64;
        edge_size - self.hidden_visible_height() > f64::EPSILON
    }

    pub(super) fn prepare_hide(&mut self, window: &WindowRef) -> bool {
        if !self.can_hide() {
            return false;
        }
        if self.hide.origin.is_none() {
            self.hide.origin = Some((self.geom.win_x, self.geom.win_y));
            self.snap_to_top_edge(window);
        }
        true
    }

    pub(super) fn compute_island_layout(&self) -> IslandLayout {
        let dock_position = if self.geom.monitor_size.0 > 0 && self.geom.monitor_size.1 > 0 {
            self.automatic_dock_position(
                WindowPosition::new(self.geom.monitor_pos.0, self.geom.monitor_pos.1),
                WindowSize::new(self.geom.monitor_size.0, self.geom.monitor_size.1),
            )
        } else {
            DockPosition::TopCenter
        };
        let dock_bottom = if self.config.notch_mode {
            false
        } else {
            dock_position.is_bottom()
        };
        let top_offset = if self.config.notch_mode {
            0.0
        } else {
            TOP_OFFSET as f64
        };
        let island_y = if dock_bottom && self.hide.origin.is_none() {
            self.geom.os_h as f64 - TOP_OFFSET as f64 - self.springs.h.value as f64
        } else {
            top_offset
        };

        let offset_x = if dock_position.is_left() {
            TOP_OFFSET as f64
        } else if dock_position.is_right() {
            (self.geom.os_w as f64 - TOP_OFFSET as f64 - self.springs.w.value as f64).max(0.0)
        } else {
            (self.geom.os_w as f64 - self.springs.w.value as f64) / 2.0
        } + self.springs.multitask_x.value as f64;

        let edge_size = self.springs.h.value as f64;
        let hidden_visible_height = self.hidden_visible_height();
        let concealed_height = (edge_size - hidden_visible_height).max(0.0);
        let hide_distance = if concealed_height > f64::EPSILON {
            concealed_height + top_offset
        } else {
            0.0
        };
        let content_hide_ratio = if edge_size > f64::EPSILON {
            (concealed_height / edge_size) as f32
        } else {
            0.0
        };
        let hide_offset = self.springs.hide.value as f64 * hide_distance;
        let current_island_x = offset_x;
        let current_island_y = if self.config.notch_mode {
            (island_y - hide_offset).min(0.0)
        } else {
            island_y - hide_offset
        };
        let compact_content_h = self
            .compact_content_height()
            .min(self.springs.h.value)
            .max(0.0) as f64;
        let stable_base_y = if dock_bottom && self.hide.origin.is_none() {
            self.geom.os_h as f64 - TOP_OFFSET as f64 - compact_content_h
        } else {
            top_offset
        };
        let stable_island_y = if self.config.notch_mode {
            (stable_base_y - hide_offset).min(0.0)
        } else {
            stable_base_y - hide_offset
        };
        let hidden_reveal_x = current_island_x;
        let hidden_reveal_y = current_island_y + self.springs.h.value as f64 - 1.0;
        let hidden_reveal_w = self.springs.w.value as f64;
        let hidden_reveal_h = 1.0;

        IslandLayout {
            offset_x,
            secondary_left: dock_position.is_right(),
            dock_bottom,
            island_y,
            current_island_x,
            current_island_y,
            stable_island_y,
            hide_distance,
            content_hide_ratio,
            hidden_reveal_x,
            hidden_reveal_y,
            hidden_reveal_w,
            hidden_reveal_h,
        }
    }

    pub(super) fn compact_content_height(&self) -> f32 {
        let scale = self.config.compact_scale.max(f32::EPSILON);
        let activity_height = if self.components_hidden {
            0.0
        } else {
            self.activity_mgr
                .current()
                .map_or(0.0, |item| item.preferred_height)
        };
        let base_height = self.config.base_height.max(activity_height) * scale;
        let has_secondary_lyric = !self.lyrics.current_secondary_text.is_empty()
            || (!self.lyrics.old_secondary_text.is_empty() && self.lyrics.transition < 1.0);
        if self.components_hidden
            || !self.config.show_lyrics
            || !self.config.show_secondary_lyrics
            || !has_secondary_lyric
            || !matches!(
                self.mini_content(),
                Some(crate::ui::island::MiniContent::Music)
            )
        {
            return base_height;
        }

        base_height.max(crate::ui::island::mini_lyric_pair_height(
            self.config.font_size,
            scale,
        ))
    }

    fn displayed_lyric_texts(&self) -> (&str, &str) {
        let (primary, secondary) = if !self.lyrics.current_text.is_empty() {
            (
                self.lyrics.current_text.as_str(),
                self.lyrics.current_secondary_text.as_str(),
            )
        } else {
            (
                self.lyrics.old_text.as_str(),
                self.lyrics.old_secondary_text.as_str(),
            )
        };
        let secondary = if self.config.show_secondary_lyrics {
            secondary
        } else {
            ""
        };
        (primary, secondary)
    }

    pub(super) fn measure_lyric_text_width(&self, text: &str) -> f32 {
        let scale = self.config.compact_scale.max(f32::EPSILON);
        let font_size = crate::ui::island::mini_lyric_font_size(self.config.font_size, scale);
        FontManager::global().measure_text_cached(
            text,
            font_size,
            winisland_render::FontStyle::normal(),
        ) / scale
    }

    fn measure_lyric_pair_width(&self, primary: &str, secondary: &str) -> f32 {
        self.measure_lyric_text_width(primary)
            .max(self.measure_lyric_text_width(secondary))
    }

    pub(super) fn compute_lyric_target_width(
        &mut self,
        window: &WindowRef,
        music_active: bool,
        is_paused: bool,
        dt: f32,
    ) -> f32 {
        let compact = !self.expanded;
        let activity_width = self
            .activity_mgr
            .current()
            .map(|item| item.preferred_width)
            .filter(|width| *width > 0.0);
        let target_base_w = if compact
            && !self.components_hidden
            && !self.is_width_hiding()
            && activity_width.is_some()
        {
            self.lyrics.scroll_offset = 0.0;
            activity_width
                .unwrap_or(self.config.base_width)
                .max(self.config.base_width)
        } else if music_active
            && matches!(
                self.mini_content(),
                Some(crate::ui::island::MiniContent::Music)
            )
            && !self.components_hidden
            && compact
            && !self.is_width_hiding()
        {
            let has_visible_lyrics = self.config.show_lyrics
                && (!self.lyrics.current_text.is_empty()
                    || (!self.lyrics.old_text.is_empty() && self.lyrics.transition < 1.0));

            if has_visible_lyrics {
                let (left_inset, right_inset) =
                    crate::ui::island::mini_lyric_insets(self.config.lyrics_side_gap);
                let horizontal_insets = left_inset + right_inset;
                if self.config.lyrics_scroll {
                    let (primary, secondary) = self.displayed_lyric_texts();
                    let primary_w = self.measure_lyric_text_width(primary);
                    let text_w = primary_w.max(self.measure_lyric_text_width(secondary));
                    let natural_w = horizontal_insets + text_w;
                    let max_w = self
                        .config
                        .lyrics_scroll_max_width
                        .max(horizontal_insets + 32.0);
                    if natural_w > max_w {
                        let fixed_w = max_w;
                        let available_text_w =
                            (fixed_w - horizontal_insets) * self.config.compact_scale;
                        let full_primary_w = primary_w * self.config.compact_scale;
                        let overflow = full_primary_w - available_text_w;
                        if overflow > 0.0 && self.lyrics.transition >= 1.0 && !is_paused {
                            self.lyrics.scroll_offset = self.lyrics.scroll_offset.min(overflow);
                            if self.lyrics.scroll_offset < overflow {
                                if self.lyrics.scroll_pause > 0.0 {
                                    self.lyrics.scroll_pause -= dt / 60.0;
                                } else {
                                    self.lyrics.scroll_offset += 0.8 * dt;
                                    if self.lyrics.scroll_offset >= overflow {
                                        self.lyrics.scroll_offset = overflow;
                                    }
                                }
                                window.request_redraw();
                            }
                        } else {
                            self.lyrics.scroll_offset = 0.0;
                        }
                        fixed_w
                    } else {
                        self.lyrics.scroll_offset = 0.0;
                        let min_w = self.config.base_width + 35.0;
                        natural_w.clamp(min_w.min(max_w), max_w)
                    }
                } else {
                    let (primary, secondary) = self.displayed_lyric_texts();
                    let text_w = self.measure_lyric_pair_width(primary, secondary);
                    self.lyrics.scroll_offset = 0.0;
                    let min_w = self.config.base_width + 35.0;
                    let w = horizontal_insets + text_w;
                    w.clamp(min_w.min(MAX_LYRIC_WIDTH), MAX_LYRIC_WIDTH)
                }
            } else {
                self.config.base_width + 35.0
            }
        } else {
            self.lyrics.scroll_offset = 0.0;
            self.config.base_width
        };
        if self.expanded {
            self.config.expanded_width * self.config.expanded_scale
        } else {
            target_base_w * self.config.compact_scale
        }
    }

    pub(super) fn island_path(&self, island: Rect, radius: f32) -> Path {
        if self.config.notch_mode {
            let ear_r = (8.0 * self.config.compact_scale).min(radius).max(2.0);
            Path::notch(island, radius, ear_r)
        } else {
            Path::continuous_rounded_rect(island, radius)
        }
    }

    pub(super) fn is_point_in_island(
        &self,
        point: (f64, f64),
        rect: (f64, f64, f64, f64),
        radius: f64,
    ) -> bool {
        if self.config.notch_mode {
            let ear_r = (8.0 * self.config.compact_scale)
                .min(radius as f32)
                .max(2.0) as f64;
            crate::utils::mouse::is_point_in_notch(point, rect, radius, ear_r)
        } else {
            let (px, py) = point;
            let (rx, ry, rw, rh) = rect;
            crate::utils::mouse::is_point_in_continuous_rounded_rect(px, py, rx, ry, rw, rh, radius)
        }
    }
}
