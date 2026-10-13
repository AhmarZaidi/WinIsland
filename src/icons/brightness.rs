use std::cell::LazyCell;

use winisland_render::{Image, ImageOptions, Painter, Point, Rect};

use super::load_glyph;

thread_local! {
    static SUN_ICONS: LazyCell<[Option<Image>; 2]> = LazyCell::new(|| [
        load_glyph(include_bytes!("../../resources/in_app/icons/sun.min.png")),
        load_glyph(include_bytes!("../../resources/in_app/icons/sun.max.png")),
    ]);
}

pub fn draw_brightness_icon(painter: Painter<'_>, center: Point, size: f32, alpha: u8, level: f32) {
    let index = usize::from(level >= 0.5);
    SUN_ICONS.with(|icons| {
        if let Some(icon) = icons[index].as_ref() {
            painter.draw_image(
                icon,
                Rect::from_xywh(center.x - size / 2.0, center.y - size / 2.0, size, size),
                &ImageOptions::default().with_alpha(alpha),
            );
        }
    });
}
