use std::cell::RefCell;
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use winisland_core::i18n::tr;
use winisland_render::text::FontManager;
use winisland_render::{Angle, Painter, Path, Point, Rect, Rgba, StrokeCap, Vec2};

use crate::ui::rolling_time::{RollingTime, TimeAnchor};

use super::CompactSize;
use super::notification::NotificationIndicator;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerContent {
    pub remaining: Duration,
    pub total: Duration,
    pub paused: bool,
}

impl TimerContent {
    pub fn remaining_secs(self) -> u64 {
        self.remaining.as_secs() + u64::from(self.remaining.subsec_nanos() > 0)
    }

    pub fn progress(self) -> f32 {
        if self.total.is_zero() {
            return 1.0;
        }
        (1.0 - self.remaining.as_secs_f32() / self.total.as_secs_f32()).clamp(0.0, 1.0)
    }

    pub fn time_text(self) -> String {
        let remaining = self.remaining_secs();
        let hours = remaining / 3600;
        let minutes = remaining % 3600 / 60;
        let seconds = remaining % 60;
        if hours > 0 {
            format!("{hours}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes}:{seconds:02}")
        }
    }
}

const YELLOW: Rgba = Rgba::from_rgb(255, 204, 0);
const AMBER: Rgba = Rgba::from_rgb(255, 179, 64);
const FONT_SIZE: f32 = 13.0;
const ICON_SIZE: f32 = 18.0;
const EDGE_INSET: f32 = 9.0;
const ENTER_DURATION: Duration = Duration::from_millis(220);
const CLOSE_DURATION: Duration = Duration::from_millis(180);

thread_local! {
    static COUNTDOWN: RefCell<RollingTime> = const { RefCell::new(RollingTime::new()) };
    static FINISHED_ICON: Path = load_finished_icon().expect("Embedded alarm SVG is valid");
}

pub(crate) fn countdown_is_animating() -> bool {
    COUNTDOWN.with(|time| time.borrow().is_animating())
}

fn draw_progress(painter: Painter<'_>, center: Point, scale: f32, fraction: f32, color: Rgba) {
    let radius = (ICON_SIZE / 2.0 - 1.6) * scale;
    let ring_width = 1.8 * scale;
    painter.stroke_circle(
        center,
        radius,
        ring_width,
        color.with_alpha_f(f32::from(color.a()) / 255.0 * 0.22),
    );
    if fraction > 0.0 {
        painter.stroke_arc(
            Rect::from_xywh(
                center.x - radius,
                center.y - radius,
                radius * 2.0,
                radius * 2.0,
            ),
            Angle::ZERO,
            Angle::from_degrees(fraction * 360.0),
            ring_width,
            color,
            StrokeCap::Round,
        );
    }
    let angle = fraction * std::f32::consts::TAU;
    let direction = Vec2::new(angle.sin(), -angle.cos());
    let marker_width = 1.5 * scale;
    let outer = radius - ring_width / 2.0 - marker_width / 2.0 - 0.8 * scale;
    let inner = outer - 2.2 * scale;
    painter.stroke_line(
        Point::new(
            center.x + direction.x * inner,
            center.y + direction.y * inner,
        ),
        Point::new(
            center.x + direction.x * outer,
            center.y + direction.y * outer,
        ),
        marker_width,
        color,
        StrokeCap::Round,
    );
}

fn load_finished_icon() -> Option<Path> {
    let mut reader = quick_xml::Reader::from_str(include_str!(
        "../../../resources/in_app/icons/alarm.fill.svg"
    ));
    loop {
        match reader.read_event().ok()? {
            Event::Empty(element) if element.name().as_ref() == b"path" => {
                let data = element.try_get_attribute("d").ok()??;
                return Path::from_svg(std::str::from_utf8(data.value.as_ref()).ok()?);
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn draw_finished_icon(painter: Painter<'_>, center: Point, scale: f32, color: Rgba) {
    let saved = painter.save();
    painter.translate(Vec2::new(center.x, center.y));
    painter.scale(Vec2::new(scale, scale));
    painter.translate(Vec2::new(-16.0, -16.0));
    FINISHED_ICON.with(|path| painter.fill_path(path, color));
    painter.restore_to(saved);
}

fn draw_text(painter: Painter<'_>, text: &str, rect: Rect, size: f32, bold: bool, color: Rgba) {
    let fonts = FontManager::global();
    let width = fonts.measure_str(text, size, bold).width();
    let size = if width > rect.width() && width > 0.0 {
        size * rect.width().max(0.0) / width
    } else {
        size
    };
    let bounds = fonts.measure_str(text, size, bold);
    fonts.draw_str(
        painter,
        text,
        Point::new(
            rect.left - bounds.left,
            rect.center_y() - bounds.height() / 2.0 - bounds.top,
        ),
        size,
        bold,
        color,
    );
}

pub(crate) fn countdown_width(timer: TimerContent, base_width: f32) -> f32 {
    let width = RollingTime::measure_width(&timer.time_text(), FONT_SIZE);
    base_width.max(EDGE_INSET * 2.0 + ICON_SIZE + 16.0 + width)
}

pub(crate) fn draw_countdown(
    painter: Painter<'_>,
    timer: TimerContent,
    rect: Rect,
    scale: f32,
    alpha: f32,
) {
    let color = YELLOW.with_alpha_f(alpha * if timer.paused { 0.65 } else { 1.0 });
    draw_progress(
        painter,
        Point::new(
            rect.left + (EDGE_INSET + ICON_SIZE / 2.0) * scale,
            rect.center_y(),
        ),
        scale,
        timer.progress(),
        color,
    );
    COUNTDOWN.with(|time| {
        time.borrow_mut().draw(
            painter,
            &timer.time_text(),
            TimeAnchor::Right(Point::new(rect.right - EDGE_INSET * scale, rect.center_y())),
            FONT_SIZE * scale,
            color,
        );
    });
}

#[derive(Default)]
pub(super) struct TimerFinishedIndicator {
    shown_at: Option<Instant>,
    closing_at: Option<Instant>,
    entered: bool,
}

impl TimerFinishedIndicator {
    pub(super) fn show(&mut self) {
        self.shown_at = Some(Instant::now());
        self.closing_at = None;
        self.entered = false;
    }

    pub(super) fn is_visible(&self) -> bool {
        self.shown_at.is_some()
            && self
                .closing_at
                .is_none_or(|started| started.elapsed() < CLOSE_DURATION)
    }

    pub(super) fn is_animating(&self) -> bool {
        self.shown_at
            .is_some_and(|started| started.elapsed() < ENTER_DURATION)
            || self
                .closing_at
                .is_some_and(|started| started.elapsed() < CLOSE_DURATION)
    }

    pub(super) fn close(&mut self) -> bool {
        if !self.is_visible() || self.closing_at.is_some() {
            return false;
        }
        self.closing_at = Some(Instant::now());
        true
    }

    pub(super) fn clear(&mut self) {
        self.shown_at = None;
        self.closing_at = None;
        self.entered = false;
    }

    pub(super) fn update(&mut self) -> bool {
        if self.shown_at.is_some()
            && self
                .closing_at
                .is_some_and(|started| started.elapsed() >= CLOSE_DURATION)
        {
            self.clear();
            return true;
        }
        if !self.entered
            && self
                .shown_at
                .is_some_and(|started| started.elapsed() >= ENTER_DURATION)
        {
            self.entered = true;
            return true;
        }
        false
    }

    pub(super) fn target_size(base_width: f32, base_height: f32, scale: f32) -> CompactSize {
        NotificationIndicator::target_size(base_width, base_height, scale)
    }

    fn close_rect(rect: Rect, scale: f32) -> Rect {
        let size = 52.0 * scale;
        let inset = ((rect.height() - size) / 2.0).max(12.0 * scale);
        Rect::from_xywh(
            rect.right - inset - size,
            rect.center_y() - size / 2.0,
            size,
            size,
        )
    }

    pub(super) fn hit_close(&self, point: Point, rect: Rect, scale: f32) -> bool {
        self.is_visible()
            && self.closing_at.is_none()
            && Self::close_rect(self.presentation(rect, scale).0, scale)
                .inset(-3.0 * scale)
                .contains(point)
    }

    fn presentation(&self, rect: Rect, scale: f32) -> (Rect, f32) {
        let Some(started) = self.shown_at else {
            return (rect, 0.0);
        };
        let enter =
            (started.elapsed().as_secs_f32() / ENTER_DURATION.as_secs_f32()).clamp(0.0, 1.0);
        let enter = 1.0 - (1.0 - enter).powi(3);
        let exit = self.closing_at.map_or(1.0, |started| {
            1.0 - (started.elapsed().as_secs_f32() / CLOSE_DURATION.as_secs_f32()).clamp(0.0, 1.0)
        });
        (
            rect.offset(Vec2::new(0.0, (1.0 - enter) * 4.0 * scale)),
            enter * exit,
        )
    }

    pub(super) fn draw(
        &self,
        painter: Painter<'_>,
        rect: Rect,
        scale: f32,
        alpha: f32,
        is_light: bool,
    ) {
        let (rect, opacity) = self.presentation(rect, scale);
        let opacity = alpha * opacity;
        if opacity <= 0.0 {
            return;
        }
        let color = AMBER.with_alpha_f(opacity);
        let button = Self::close_rect(rect, scale);
        let icon_center = Point::new(rect.left + 46.0 * scale, rect.center_y());
        draw_finished_icon(painter, icon_center, 1.7 * scale, color);
        let text_left = icon_center.x + 31.0 * scale;
        draw_text(
            painter,
            &tr("timer_done"),
            Rect::from_xywh(
                text_left,
                icon_center.y + 4.0 * scale,
                button.left - text_left - 16.0 * scale,
                22.0 * scale,
            ),
            18.0 * scale,
            false,
            color,
        );
        let center = Point::new(button.center_x(), button.center_y());
        let circle_color = if is_light {
            Rgba::BLACK.with_alpha_f(opacity * 0.08)
        } else {
            Rgba::WHITE.with_alpha_f(opacity * 0.12)
        };
        painter.fill_circle(center, button.width() / 2.0, circle_color);
        let arm = 8.0 * scale;
        let cross_color = if is_light {
            Rgba::BLACK.with_alpha_f(opacity * 0.65)
        } else {
            Rgba::WHITE.with_alpha_f(opacity * 0.65)
        };
        for direction in [-1.0, 1.0] {
            painter.stroke_line(
                Point::new(center.x - arm, center.y - direction * arm),
                Point::new(center.x + arm, center.y + direction * arm),
                2.2 * scale,
                cross_color,
                StrokeCap::Round,
            );
        }
    }
}
