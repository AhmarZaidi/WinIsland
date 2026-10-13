use std::time::{Duration, Instant};

use winisland_render::text::FontManager;
use winisland_render::{Painter, Point, Rect, Rgba, Vec2};

use crate::ui::widget::expanded::draw_widget_text_centered;

const DIGIT_DURATION: Duration = Duration::from_millis(300);
const MAX_DIGITS: usize = 8;
const WIDTH_RATE: f32 = 14.0;

#[derive(Clone, Copy)]
struct DigitChange {
    previous: char,
    started: Instant,
}

pub(crate) enum TimeAnchor {
    Center(Point),
    Right(Point),
}

pub(crate) struct RollingTime {
    digits: [char; MAX_DIGITS],
    digit_count: usize,
    changes: [Option<DigitChange>; MAX_DIGITS],
    departing: [Option<DigitChange>; MAX_DIGITS],
    width: f32,
    target_width: f32,
    last_frame: Option<Instant>,
}

impl RollingTime {
    pub(crate) const fn new() -> Self {
        Self {
            digits: [' '; MAX_DIGITS],
            digit_count: 0,
            changes: [None; MAX_DIGITS],
            departing: [None; MAX_DIGITS],
            width: 0.0,
            target_width: 0.0,
            last_frame: None,
        }
    }

    pub(crate) fn is_animating(&self) -> bool {
        self.changes
            .iter()
            .chain(self.departing.iter())
            .any(Option::is_some)
            || (self.width - self.target_width).abs() > 0.25
    }

    pub(crate) fn measure_width(text: &str, size: f32) -> f32 {
        let digit_width = FontManager::global().measure_str("0", size, true).width();
        text.chars()
            .map(|character| character_width(character, size, digit_width))
            .sum()
    }

    fn advance(&mut self, text: &str, now: Instant) {
        for change in self.changes.iter_mut().chain(self.departing.iter_mut()) {
            if change.is_some_and(|change| now.duration_since(change.started) >= DIGIT_DURATION) {
                *change = None;
            }
        }
        let chars: Vec<char> = text.chars().take(MAX_DIGITS).collect();
        let old_count = self.digit_count;
        let new_count = chars.len();
        if old_count > 0 && old_count != new_count {
            let mut shifted = [None; MAX_DIGITS];
            for (index, slot) in shifted.iter_mut().enumerate().take(new_count) {
                let old_index = (index + old_count).checked_sub(new_count);
                *slot = old_index
                    .filter(|old_index| *old_index < old_count)
                    .and_then(|old_index| self.changes[old_index]);
            }
            self.changes = shifted;
            if new_count < old_count {
                self.departing = [None; MAX_DIGITS];
                for index in 0..old_count - new_count {
                    self.departing[index] = Some(DigitChange {
                        previous: self.digits[index],
                        started: now,
                    });
                }
            }
        }
        for (index, character) in chars.iter().enumerate() {
            let old_index = (index + old_count).checked_sub(new_count);
            let previous = match old_index {
                Some(old_index) if old_index < old_count => self.digits[old_index],
                _ if old_count > 0 => ' ',
                _ => *character,
            };
            if previous != *character {
                self.changes[index] = Some(DigitChange {
                    previous,
                    started: now,
                });
            }
        }
        self.digit_count = new_count;
        self.digits[..new_count].copy_from_slice(&chars);
    }

    pub(crate) fn draw(
        &mut self,
        painter: Painter<'_>,
        text: &str,
        anchor: TimeAnchor,
        size: f32,
        color: Rgba,
    ) {
        self.draw_with(painter, text, anchor, size, |_| color);
    }

    pub(crate) fn draw_with(
        &mut self,
        painter: Painter<'_>,
        text: &str,
        anchor: TimeAnchor,
        size: f32,
        color: impl Fn(usize) -> Rgba,
    ) {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map_or(0.0, |last| now.duration_since(last).as_secs_f32())
            .min(0.1);
        self.last_frame = Some(now);
        self.advance(text, now);
        let digit_width = FontManager::global().measure_str("0", size, true).width();
        let advance = |character| character_width(character, size, digit_width);
        let chars = &self.digits[..self.digit_count];
        let total: f32 = chars.iter().map(|character| advance(*character)).sum();
        self.target_width = total;
        if self.width <= f32::EPSILON {
            self.width = total;
        } else {
            self.width += (total - self.width) * (1.0 - (-dt * WIDTH_RATE).exp());
            if (total - self.width).abs() <= 0.25 {
                self.width = total;
            }
        }
        let right = match anchor {
            TimeAnchor::Center(center) => Point::new(center.x + self.width / 2.0, center.y),
            TimeAnchor::Right(right) => right,
        };
        let mut x = right.x - total;
        let travel = size * 0.55;
        let save_count = painter.save();
        painter.clip_rect(Rect::from_xywh(
            right.x - total.max(self.width) * 1.5,
            right.y - size * 0.62,
            total.max(self.width) * 2.0,
            size * 1.24,
        ));
        let opacity = |color: Rgba| f32::from(color.a()) / 255.0;
        let eased = |change: DigitChange| {
            let t = (now.duration_since(change.started).as_secs_f32()
                / DIGIT_DURATION.as_secs_f32())
            .clamp(0.0, 1.0);
            1.0 - (1.0 - t).powi(3)
        };
        let mut buffer = [0u8; 4];
        let mut departing_x = x;
        for change in self.departing.iter().flatten().rev() {
            let width = advance(change.previous);
            departing_x -= width;
            let amount = eased(*change);
            let color = color(0);
            draw_widget_text_centered(
                painter,
                change.previous.encode_utf8(&mut buffer),
                Rect::from_xywh(
                    departing_x,
                    right.y - size / 2.0 - travel * amount,
                    width,
                    size,
                ),
                size,
                true,
                color.with_alpha_f(opacity(color) * (1.0 - amount)),
            );
        }
        for (index, character) in chars.iter().enumerate() {
            let width = advance(*character);
            let cell = Rect::from_xywh(x, right.y - size / 2.0, width, size);
            let color = color(index);
            if let Some(change) = self.changes[index] {
                let amount = eased(change);
                draw_widget_text_centered(
                    painter,
                    change.previous.encode_utf8(&mut buffer),
                    cell.offset(Vec2::new(0.0, -travel * amount)),
                    size,
                    true,
                    color.with_alpha_f(opacity(color) * (1.0 - amount)),
                );
                draw_widget_text_centered(
                    painter,
                    character.encode_utf8(&mut buffer),
                    cell.offset(Vec2::new(0.0, travel * (1.0 - amount))),
                    size,
                    true,
                    color.with_alpha_f(opacity(color) * amount),
                );
            } else {
                draw_widget_text_centered(
                    painter,
                    character.encode_utf8(&mut buffer),
                    cell,
                    size,
                    true,
                    color,
                );
            }
            x += width;
        }
        painter.restore_to(save_count);
    }
}

fn character_width(character: char, size: f32, digit_width: f32) -> f32 {
    if character.is_ascii_digit() {
        digit_width
    } else {
        FontManager::global()
            .measure_str(&character.to_string(), size, true)
            .width()
            .max(size * 0.25)
    }
}
