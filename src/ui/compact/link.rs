use std::time::{Duration, Instant};

use winisland_core::i18n::tr;
use winisland_render::text::FontManager;
use winisland_render::{Painter, Point, Radius, Rect, Rgba, StrokeCap, Vec2};

use crate::ui::compact::CompactSize;

const DISPLAY_DURATION: Duration = Duration::from_secs(7);
const ENTER_DURATION: Duration = Duration::from_millis(260);
const FADE_DURATION: Duration = Duration::from_millis(240);
const MAX_LINK_CHARS: usize = 2048;
const BLUE: Rgba = Rgba::from_rgb(10, 132, 255);
const ICON_SIZE: f32 = 36.0;
const EDGE_INSET: f32 = 14.0;
const BUTTON_WIDTH: f32 = 64.0;
const BUTTON_HEIGHT: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkHit {
    Open,
    Dismiss,
}

#[derive(Default)]
pub(super) struct LinkIndicator {
    url: String,
    host: String,
    shown_at: Option<Instant>,
    closing_at: Option<Instant>,
}

pub(crate) fn extract_link(text: &str) -> Option<String> {
    let candidate = text
        .trim()
        .trim_matches(|character: char| matches!(character, '<' | '>' | '"' | '\'' | '(' | ')'));
    if candidate.is_empty()
        || candidate.chars().count() > MAX_LINK_CHARS
        || candidate.chars().any(char::is_whitespace)
    {
        return None;
    }
    let lower = candidate.to_ascii_lowercase();
    let url = if lower.starts_with("https://") || lower.starts_with("http://") {
        candidate.to_string()
    } else if lower.starts_with("www.") {
        format!("https://{candidate}")
    } else {
        return None;
    };
    let host = host_of(&url);
    (host.contains('.') || host.eq_ignore_ascii_case("localhost")).then_some(url)
}

fn host_of(url: &str) -> String {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

fn ease_out_cubic(value: f32) -> f32 {
    1.0 - (1.0 - value).powi(3)
}

fn truncated(text: &str, size: f32, bold: bool, max_width: f32) -> String {
    let fonts = FontManager::global();
    if fonts.measure_str(text, size, bold).width() <= max_width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate = chars.iter().collect::<String>() + "…";
        if fonts.measure_str(&candidate, size, bold).width() <= max_width {
            return candidate;
        }
    }
    String::new()
}

fn draw_text(
    painter: Painter<'_>,
    text: &str,
    origin: Point,
    size: f32,
    bold: bool,
    color: Rgba,
) -> f32 {
    let bounds = FontManager::global().measure_str(text, size, bold);
    FontManager::global().draw_str(
        painter,
        text,
        Point::new(origin.x - bounds.left, origin.y),
        size,
        bold,
        color,
    );
    bounds.width()
}

impl LinkIndicator {
    pub(super) fn show(&mut self, url: String) {
        self.host = host_of(&url);
        self.url = url;
        self.shown_at = Some(Instant::now());
        self.closing_at = None;
    }

    fn display_until(&self) -> Option<Instant> {
        self.closing_at
            .or_else(|| self.shown_at.map(|shown| shown + DISPLAY_DURATION))
    }

    pub(super) fn is_visible(&self) -> bool {
        self.display_until()
            .is_some_and(|until| until + FADE_DURATION > Instant::now())
    }

    pub(super) fn is_interactive(&self) -> bool {
        self.display_until()
            .is_some_and(|until| until > Instant::now())
    }

    pub(super) fn close(&mut self) {
        if self.is_interactive() {
            self.closing_at = Some(Instant::now());
        }
    }

    pub(super) fn take_url(&mut self) -> Option<String> {
        if !self.is_interactive() {
            return None;
        }
        self.close();
        Some(std::mem::take(&mut self.url))
    }

    pub(super) fn target_size(base_width: f32, base_height: f32, scale: f32) -> CompactSize {
        CompactSize {
            width: base_width.max(320.0) * scale,
            height: base_height.max(62.0) * scale,
        }
    }

    fn edge_inset(rect: Rect, size: f32, scale: f32) -> f32 {
        ((rect.height() - size) / 2.0).max(EDGE_INSET * scale)
    }

    fn button_rect(rect: Rect, scale: f32) -> Rect {
        let height = BUTTON_HEIGHT * scale;
        let inset = Self::edge_inset(rect, height, scale);
        Rect::from_xywh(
            rect.right - inset - BUTTON_WIDTH * scale,
            rect.center_y() - height / 2.0,
            BUTTON_WIDTH * scale,
            height,
        )
    }

    pub(super) fn hit(&self, x: f32, y: f32, rect: Rect, scale: f32) -> LinkHit {
        if Self::button_rect(rect, scale)
            .inset(-4.0 * scale)
            .contains(Point::new(x, y))
        {
            LinkHit::Open
        } else {
            LinkHit::Dismiss
        }
    }

    fn presentation(&self) -> (f32, f32) {
        let (Some(started), Some(until)) = (self.shown_at, self.display_until()) else {
            return (0.0, 0.0);
        };
        let now = Instant::now();
        let enter = ease_out_cubic(
            (now.saturating_duration_since(started).as_secs_f32() / ENTER_DURATION.as_secs_f32())
                .clamp(0.0, 1.0),
        );
        let exit_elapsed = now.saturating_duration_since(until);
        let exit = if exit_elapsed.is_zero() {
            1.0
        } else {
            (1.0 - exit_elapsed.as_secs_f32() / FADE_DURATION.as_secs_f32()).clamp(0.0, 1.0)
        };
        (enter * exit, (1.0 - enter) * 7.0)
    }

    pub(super) fn draw(
        &self,
        painter: Painter<'_>,
        rect: Rect,
        scale: f32,
        alpha: f32,
        is_light: bool,
    ) {
        let (opacity, offset_y) = self.presentation();
        let opacity = (alpha * opacity).clamp(0.0, 1.0);
        if opacity <= 0.01 {
            return;
        }
        painter.save();
        painter.translate(Vec2::new(0.0, offset_y * scale));

        let icon_radius = ICON_SIZE * scale / 2.0;
        let icon_center = Point::new(
            rect.left + Self::edge_inset(rect, icon_radius * 2.0, scale) + icon_radius,
            rect.center_y(),
        );
        painter.fill_circle(icon_center, icon_radius, BLUE.with_alpha_f(opacity));
        let arm = icon_radius * 0.38;
        let glyph = Rgba::WHITE.with_alpha_f(opacity);
        let stroke = 1.8 * scale;
        let tip = Point::new(icon_center.x + arm, icon_center.y - arm);
        painter.stroke_line(
            Point::new(icon_center.x - arm * 0.35, icon_center.y + arm * 0.35),
            tip,
            stroke,
            glyph,
            StrokeCap::Round,
        );
        painter.stroke_line(
            tip,
            Point::new(tip.x - arm * 0.9, tip.y),
            stroke,
            glyph,
            StrokeCap::Round,
        );
        painter.stroke_line(
            tip,
            Point::new(tip.x, tip.y + arm * 0.9),
            stroke,
            glyph,
            StrokeCap::Round,
        );
        let corner = Point::new(icon_center.x - arm, icon_center.y + arm);
        painter.stroke_line(
            Point::new(corner.x, corner.y - arm * 0.95),
            corner,
            stroke,
            glyph,
            StrokeCap::Round,
        );
        painter.stroke_line(
            corner,
            Point::new(corner.x + arm * 0.95, corner.y),
            stroke,
            glyph,
            StrokeCap::Round,
        );

        let button = Self::button_rect(rect, scale);
        let text_left = icon_center.x + icon_radius + 12.0 * scale;
        let text_width = (button.left - 12.0 * scale - text_left).max(0.0);
        let title = truncated(&tr("link_prompt_title"), 11.0 * scale, false, text_width);
        let (title_color, host_color) = if is_light {
            (
                Rgba::from_rgb(100, 100, 105).with_alpha_f(opacity),
                Rgba::from_rgb(15, 15, 18).with_alpha_f(opacity),
            )
        } else {
            (
                Rgba::WHITE.with_alpha_f(0.65 * opacity),
                Rgba::WHITE.with_alpha_f(opacity),
            )
        };
        draw_text(
            painter,
            &title,
            Point::new(text_left, rect.center_y() - 4.0 * scale),
            11.0 * scale,
            false,
            title_color,
        );
        let host = truncated(&self.host, 14.0 * scale, true, text_width);
        draw_text(
            painter,
            &host,
            Point::new(text_left, rect.center_y() + 13.0 * scale),
            14.0 * scale,
            true,
            host_color,
        );

        painter.fill_round_rect(
            button,
            Radius::uniform(button.height() / 2.0),
            BLUE.with_alpha_f(opacity),
        );
        let label = tr("link_prompt_open");
        let label_size = 12.0 * scale;
        let label_bounds = FontManager::global().measure_str(&label, label_size, true);
        FontManager::global().draw_str(
            painter,
            &label,
            Point::new(
                button.center_x() - label_bounds.width() / 2.0 - label_bounds.left,
                button.center_y() - label_bounds.height() / 2.0 - label_bounds.top,
            ),
            label_size,
            true,
            Rgba::WHITE.with_alpha_f(opacity),
        );
        painter.restore();
    }
}
