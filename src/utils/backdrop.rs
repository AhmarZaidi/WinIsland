use std::cell::RefCell;

use winisland_render::{
    BlurSpec, Image, ImageOptions, LayerSpec, RasterSurface, Rect, Rgba, Sampling, TileMode,
};

use crate::core::smtc::MediaInfo;
use crate::ui::expanded::music_view::get_cached_media_image_with_key;

const BLURRED_COVER_SIZE: i32 = 64;
const BLURRED_COVER_SIGMA: f32 = 8.0;

thread_local! {
    static BLURRED_COVER_CACHE: RefCell<Option<BlurredCoverCache>> = const { RefCell::new(None) };
}

struct BlurredCoverCache {
    cache_key: u64,
    blurred_image: Image,
}

pub fn get_blurred_cover_background(media: &MediaInfo) -> Option<Image> {
    if media.title.is_empty() {
        return None;
    }
    let (image, cache_key) = get_cached_media_image_with_key(media)?;

    let cached = BLURRED_COVER_CACHE.with(|cell| {
        let cache = cell.borrow();
        cache
            .as_ref()
            .filter(|entry| entry.cache_key == cache_key)
            .map(|entry| entry.blurred_image.clone())
    });
    if cached.is_some() {
        return cached;
    }

    let blurred_image = blur_cover(&image)?;

    BLURRED_COVER_CACHE.with(|cell| {
        *cell.borrow_mut() = Some(BlurredCoverCache {
            cache_key,
            blurred_image: blurred_image.clone(),
        });
    });

    Some(blurred_image)
}

fn blur_cover(image: &Image) -> Option<Image> {
    let mut surface = RasterSurface::new(BLURRED_COVER_SIZE, BLURRED_COVER_SIZE)?;
    surface.clear(Rgba::TRANSPARENT);
    let painter = surface.painter();
    painter.begin_layer(LayerSpec::Blur(BlurSpec {
        sigma: (BLURRED_COVER_SIGMA, BLURRED_COVER_SIGMA),
        tile: Some(TileMode::Clamp),
    }));
    painter.draw_image(
        image,
        Rect::from_xywh(
            0.0,
            0.0,
            BLURRED_COVER_SIZE as f32,
            BLURRED_COVER_SIZE as f32,
        ),
        &ImageOptions::default().with_sampling(Sampling::LinearNone),
    );
    painter.end_layer();
    Some(surface.snapshot())
}

pub fn clear_blurred_cover_cache() {
    BLURRED_COVER_CACHE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
