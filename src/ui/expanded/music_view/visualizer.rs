use std::cell::RefCell;
use std::sync::Arc;

use winisland_render::{
    GradientStop, Image, ImageOptions, Painter, Point, Radius, Rect, Rgba, Sampling, TileMode,
};

thread_local! {
    static VIZ_HEIGHTS: RefCell<[f32; 6]> = const { RefCell::new([3.0; 6]) };
}

#[derive(Clone)]
pub enum SpectrumFill {
    Cover(Image),
    Colors(Arc<[Rgba]>),
}

pub struct DrawVisualizerParams<'a> {
    pub painter: Painter<'a>,
    pub x: f32,
    pub y: f32,
    pub alpha: u8,
    pub is_playing: bool,
    pub fill: &'a SpectrumFill,
    pub spectrum: &'a [f32; 6],
    pub w_scale: f32,
    pub h_scale: f32,
    pub smooth_factors: (f32, f32),
}

pub fn draw_visualizer(params: DrawVisualizerParams<'_>) {
    if !crate::platform::capabilities().audio_loopback {
        return;
    }
    let DrawVisualizerParams {
        painter,
        x,
        y,
        alpha,
        is_playing,
        fill,
        spectrum,
        w_scale,
        h_scale,
        smooth_factors,
    } = params;

    let (rise, fall) = smooth_factors;
    let bar_count = 6;
    let bar_w = 3.0 * w_scale;
    let spacing = 2.0 * w_scale;
    let max_h = 28.0 * h_scale;
    VIZ_HEIGHTS.with(|h_cell| {
        let mut heights = h_cell.borrow_mut();
        for i in 0..bar_count {
            let target = if is_playing {
                (spectrum[i] * max_h).max(3.0 * h_scale)
            } else {
                3.0 * h_scale
            };
            if target > heights[i] {
                heights[i] = heights[i] * (1.0 - rise) + target * rise;
            } else {
                heights[i] = heights[i] * (1.0 - fall) + target * fall;
            }
            heights[i] = heights[i].max(3.0 * h_scale);
        }
        let start_x = x - (bar_count as f32 * (bar_w + spacing)) / 2.0;
        let span = bar_count as f32 * (bar_w + spacing) - spacing;
        let bounds = Rect::from_xywh(start_x, y - max_h / 2.0, span, max_h);
        let radius = Radius::uniform(bar_w / 2.0);
        let bars = (0..bar_count).map(|i| {
            let h = heights[i];
            Rect::from_xywh(
                start_x + i as f32 * (bar_w + spacing),
                y - h / 2.0,
                bar_w,
                h,
            )
        });
        match fill {
            SpectrumFill::Cover(image) => {
                let options = ImageOptions::default()
                    .with_sampling(Sampling::LinearNone)
                    .with_alpha(alpha);
                for rect in bars {
                    let save_count = painter.save();
                    painter.clip_round_rect(rect, radius);
                    painter.draw_image(image, bounds, &options);
                    painter.restore_to(save_count);
                }
            }
            SpectrumFill::Colors(colors) => {
                let first = colors
                    .first()
                    .copied()
                    .unwrap_or(Rgba::WHITE)
                    .with_alpha(alpha);
                let last_stop = colors.len().saturating_sub(1).max(1) as f32;
                let stops: Vec<_> = colors
                    .iter()
                    .enumerate()
                    .map(|(index, color)| GradientStop {
                        offset: index as f32 / last_stop,
                        color: color.with_alpha(alpha),
                    })
                    .collect();
                let from = Point::new(bounds.left, bounds.top);
                let to = Point::new(bounds.right, bounds.bottom);
                for rect in bars {
                    if colors.len() >= 2 {
                        painter.fill_round_rect_with_gradient(
                            rect,
                            radius,
                            from,
                            to,
                            &stops,
                            TileMode::Clamp,
                        );
                    } else {
                        painter.fill_round_rect(rect, radius, first);
                    }
                }
            }
        }
    });
}
