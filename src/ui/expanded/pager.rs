use winisland_core::config::ExpandedPageKind;
use winisland_render::{
    Backdrop, BlurSpec, Image, ImageFit, ImageOptions, LiquidMaterial, LiquidShape, Painter, Path,
    Point, Radius, Rect, Rgba, Sampling, StrokeCap, Vec2,
};

const GAP: f32 = 8.0;
const HEIGHT: f32 = 14.0;
const PADDING_X: f32 = 7.0;
const DOT_SIZE: f32 = 5.0;
const DOT_GAP: f32 = 5.0;
const ACTIVE_WIDTH: f32 = 14.0;
const CLOSE_SIZE: f32 = 20.0;
const CLOSE_GAP: f32 = 6.0;
const CLOSE_ARM: f32 = 3.5;
const HIT_SLOP: f32 = 4.0;
const SHADOW_SIGMA: f32 = 4.0;
const ENTER_OFFSET: f32 = 6.0;
const INACTIVE_DOT_ALPHA: f32 = 0.42;
pub const PAGER_EXTENT: f32 = GAP + CLOSE_SIZE + SHADOW_SIGMA * 3.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExpandedPage {
    Music,
    Widgets,
    Timer,
    Plugin(u64),
}

pub struct PageAvailability {
    pub music: bool,
    pub calendar: Option<u64>,
}

impl ExpandedPage {
    pub fn is_available(self, availability: &PageAvailability) -> bool {
        match self {
            Self::Music => availability.music,
            Self::Widgets | Self::Timer | Self::Plugin(_) => true,
        }
    }
}

pub fn available_pages(
    order: &[ExpandedPageKind],
    availability: &PageAvailability,
) -> Vec<ExpandedPage> {
    order
        .iter()
        .filter_map(|kind| match kind {
            ExpandedPageKind::Music => Some(ExpandedPage::Music),
            ExpandedPageKind::Widgets => Some(ExpandedPage::Widgets),
            ExpandedPageKind::Calendar => availability.calendar.map(ExpandedPage::Plugin),
            ExpandedPageKind::Timer => Some(ExpandedPage::Timer),
        })
        .filter(|page| page.is_available(availability))
        .collect()
}

#[derive(Clone, Copy)]
pub struct PagerLayout {
    pub bar: Option<Rect>,
    pub close: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagerHit {
    Page(usize),
    Close,
}

fn layout(island: Rect, count: usize, scale: f32, above: bool) -> PagerLayout {
    let bar_width = (count > 1).then(|| {
        let inactive = count.saturating_sub(1) as f32;
        (PADDING_X * 2.0 + ACTIVE_WIDTH + inactive * (DOT_SIZE + DOT_GAP)) * scale
    });
    let height = HEIGHT * scale;
    let total = bar_width.map_or(0.0, |width| width + CLOSE_GAP * scale) + height;
    let left = island.center_x() - total / 2.0;
    let center_y = if above {
        island.top - GAP * scale - height / 2.0
    } else {
        island.bottom + GAP * scale + height / 2.0
    };
    PagerLayout {
        bar: bar_width.map(|width| Rect::from_xywh(left, center_y - height / 2.0, width, height)),
        close: centered_square(
            Point::new(left + total - height / 2.0, center_y),
            CLOSE_SIZE * scale,
        ),
    }
}

fn centered_square(center: Point, size: f32) -> Rect {
    Rect::from_xywh(center.x - size / 2.0, center.y - size / 2.0, size, size)
}

fn close_display_size(scale: f32, hover: f32) -> f32 {
    (HEIGHT + (CLOSE_SIZE - HEIGHT) * hover.clamp(0.0, 1.0)) * scale
}

pub fn visible_layout(
    island: Rect,
    count: usize,
    scale: f32,
    above: bool,
    alpha: f32,
    bar_hover: f32,
    close_hover: f32,
) -> PagerLayout {
    let toward_island = if above { 1.0 } else { -1.0 };
    let offset = Vec2::new(
        0.0,
        toward_island * (1.0 - alpha.clamp(0.0, 1.0)) * ENTER_OFFSET * scale,
    );
    let resting = layout(island, count, scale, above);
    let close = resting.close.offset(offset);
    let bar_height = close_display_size(scale, bar_hover);
    PagerLayout {
        bar: resting.bar.map(|bar| {
            let bar = bar.offset(offset);
            Rect::from_xywh(
                bar.left,
                bar.center_y() - bar_height / 2.0,
                bar.width(),
                bar_height,
            )
        }),
        close: centered_square(
            Point::new(close.center_x(), close.center_y()),
            close_display_size(scale, close_hover),
        ),
    }
}

fn dot_weight(position: f32, index: usize) -> f32 {
    (1.0 - (position - index as f32).abs()).max(0.0)
}

fn dots(rect: Rect, count: usize, position: f32, scale: f32) -> Vec<(Rect, f32)> {
    let mut x = rect.left + PADDING_X * scale;
    let size = DOT_SIZE * scale;
    (0..count)
        .map(|index| {
            let weight = dot_weight(position, index);
            let width = (DOT_SIZE + (ACTIVE_WIDTH - DOT_SIZE) * weight) * scale;
            let dot = Rect::from_xywh(x, rect.center_y() - size / 2.0, width, size);
            x += width + DOT_GAP * scale;
            (dot, weight)
        })
        .collect()
}

pub fn contains(island: Rect, count: usize, scale: f32, above: bool, point: Point) -> bool {
    hit_regions(island, count, scale, above)
        .into_iter()
        .flatten()
        .any(|rect| rect.contains(point))
}

pub fn hit_regions(island: Rect, count: usize, scale: f32, above: bool) -> [Option<Rect>; 2] {
    let layout = layout(island, count, scale, above);
    let slop = -HIT_SLOP * scale;
    [
        Some(layout.close.inset(slop)),
        layout.bar.map(|bar| bar.inset(slop)),
    ]
}

pub fn hit_test(
    island: Rect,
    count: usize,
    position: f32,
    scale: f32,
    above: bool,
    point: Point,
) -> Option<PagerHit> {
    let layout = layout(island, count, scale, above);
    let slop = -HIT_SLOP * scale;
    if layout.close.inset(slop).contains(point) {
        return Some(PagerHit::Close);
    }
    let bar = layout.bar.filter(|bar| bar.inset(slop).contains(point))?;
    dots(bar, count, position, scale)
        .iter()
        .enumerate()
        .min_by(|(_, (a, _)), (_, (b, _))| {
            (a.center_x() - point.x)
                .abs()
                .total_cmp(&(b.center_x() - point.x).abs())
        })
        .map(|(index, _)| PagerHit::Page(index))
}

pub struct PagerParams<'a> {
    pub painter: Painter<'a>,
    pub island: Rect,
    pub count: usize,
    pub position: f32,
    pub scale: f32,
    pub above: bool,
    pub alpha: f32,
    pub bar_hover: f32,
    pub close_hover: f32,
    pub host_blur: bool,
    pub backdrop: Option<&'a Image>,
    pub liquid: Option<(&'a Backdrop, crate::ui::island::LiquidOptics)>,
    pub is_light: bool,
}

pub fn draw(params: PagerParams<'_>) {
    let PagerParams {
        painter,
        island,
        count,
        position,
        scale,
        above,
        alpha,
        bar_hover,
        close_hover,
        host_blur,
        backdrop,
        liquid,
        is_light,
    } = params;
    if alpha <= 0.01 {
        return;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let close_hover = close_hover.clamp(0.0, 1.0);
    let layout = visible_layout(island, count, scale, above, alpha, bar_hover, close_hover);
    let (dot_color, cross_color) = if is_light {
        (Rgba::BLACK, Rgba::BLACK)
    } else {
        (Rgba::WHITE, Rgba::WHITE)
    };
    if let Some(bar) = layout.bar {
        draw_surface(
            painter, bar, scale, alpha, host_blur, backdrop, liquid, is_light,
        );
        for (dot, weight) in dots(bar, count, position, scale) {
            let dot_alpha = INACTIVE_DOT_ALPHA + (1.0 - INACTIVE_DOT_ALPHA) * weight;
            painter.fill_round_rect(
                dot,
                Radius::uniform(dot.height() / 2.0),
                dot_color.with_alpha_f(dot_alpha * alpha),
            );
        }
    }
    draw_surface(
        painter,
        layout.close,
        scale,
        alpha,
        host_blur,
        backdrop,
        liquid,
        is_light,
    );
    if close_hover <= 0.01 {
        return;
    }
    let center = Point::new(layout.close.center_x(), layout.close.center_y());
    let arm = CLOSE_ARM * scale * (0.6 + 0.4 * close_hover);
    let cross = cross_color.with_alpha_f(0.9 * alpha * close_hover);
    for direction in [1.0, -1.0] {
        painter.stroke_line(
            Point::new(center.x - arm, center.y - arm * direction),
            Point::new(center.x + arm, center.y + arm * direction),
            1.6 * scale,
            cross,
            StrokeCap::Round,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_surface(
    painter: Painter<'_>,
    rect: Rect,
    scale: f32,
    alpha: f32,
    host_blur: bool,
    backdrop: Option<&Image>,
    liquid: Option<(&Backdrop, crate::ui::island::LiquidOptics)>,
    is_light: bool,
) {
    let radius = rect.height() / 2.0;
    let path = Path::continuous_rounded_rect(rect, radius);

    if let Some((liquid, optics)) = liquid {
        let shape = LiquidShape {
            rect,
            radius,
            secondary: None,
            smoothing: 0.0,
        };
        let material = optics.apply(LiquidMaterial::control(scale));
        if painter.draw_liquid_glass(liquid, &shape, &material, 1.0, alpha) {
            return;
        }
    }

    painter.save();
    painter.clip_path_difference(&path);
    painter.translate(Vec2::new(0.0, 1.0 * scale));
    painter.fill_path_blurred(
        &path,
        Rgba::BLACK.with_alpha_f(0.28 * alpha),
        BlurSpec::uniform(SHADOW_SIGMA * scale),
    );
    painter.restore();

    painter.save();
    painter.clip_path(&path);
    if let Some(image) = backdrop {
        let side = rect.width().max(rect.height()) * 1.4;
        painter.draw_image(
            image,
            Rect::from_xywh(
                rect.center_x() - side / 2.0,
                rect.center_y() - side / 2.0,
                side,
                side,
            ),
            &ImageOptions::default()
                .with_fit(ImageFit::Cover)
                .with_sampling(Sampling::LinearNone)
                .with_alpha_f(alpha),
        );
        let overlay = if is_light {
            Rgba::from_rgb(240, 240, 245).with_alpha_f(0.5 * alpha)
        } else {
            Rgba::from_rgb(20, 20, 24).with_alpha_f(0.5 * alpha)
        };
        painter.fill_rect(rect, overlay);
    } else if host_blur {
        let fill = if is_light {
            Rgba::from_rgb(250, 250, 255).with_alpha_f(0.5 * alpha)
        } else {
            Rgba::from_rgb(12, 12, 16).with_alpha_f(0.38 * alpha)
        };
        painter.fill_rect(rect, fill);
    } else {
        let fill = if is_light {
            Rgba::from_rgb(240, 240, 244).with_alpha_f(0.85 * alpha)
        } else {
            Rgba::from_rgb(22, 22, 26).with_alpha_f(0.72 * alpha)
        };
        painter.fill_rect(rect, fill);
    }
    let inner_glow = if is_light {
        Rgba::BLACK.with_alpha_f(0.03 * alpha)
    } else {
        Rgba::WHITE.with_alpha_f(0.06 * alpha)
    };
    painter.fill_path_blurred(&path, inner_glow, BlurSpec::uniform(radius * 0.6));
    painter.restore();

    let border_color = if is_light {
        Rgba::BLACK.with_alpha_f(0.12 * alpha)
    } else {
        Rgba::WHITE.with_alpha_f(0.12 * alpha)
    };
    painter.stroke_round_rect(
        rect.inset(0.5),
        Radius::uniform(radius - 0.5),
        1.0,
        border_color,
    );
}
