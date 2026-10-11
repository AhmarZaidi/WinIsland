use std::cell::RefCell;
use std::collections::HashMap;

use winisland_render::{Image, Rgba};

const SAMPLE_SIZE: i32 = 24;
const GRID_COLUMNS: usize = 2;
const GRID_ROWS: usize = 3;
const SPECTRUM_IMAGE_SIZE: i32 = 16;
const HUE_BINS: usize = 12;
const NEUTRAL_BINS: usize = 3;
const NEUTRAL_CHROMA: f32 = 0.04;
const VIBRANCE_CHROMA: f32 = 0.02;
const MIN_LIGHTNESS: f32 = 0.5;
const MIN_CONTRAST: f32 = 3.0;
const BACKDROP_DIM: f32 = 120.0 / 255.0;
const BACKDROP_SHADE: f32 = 20.0;
const CACHE_LIMIT: usize = 50;

thread_local! {
    static SPECTRUM_CACHE: RefCell<HashMap<u64, Option<Image>>> = RefCell::new(HashMap::new());
}

pub(super) fn get_spectrum_image(img: &Image, cache_key: u64) -> Option<Image> {
    SPECTRUM_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.get(&cache_key) {
            return image.clone();
        }
        if cache.len() >= CACHE_LIMIT
            && let Some(oldest) = cache.keys().next().copied()
        {
            cache.remove(&oldest);
        }
        let image = spectrum_image(img);
        cache.insert(cache_key, image.clone());
        image
    })
}

fn spectrum_image(image: &Image) -> Option<Image> {
    if image.width() <= 0 || image.height() <= 0 {
        return None;
    }
    let sample = SAMPLE_SIZE as usize;
    let pixels = image.sample_bgra8(SAMPLE_SIZE, SAMPLE_SIZE)?;
    let rgb: Vec<[f32; 3]> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            let alpha = (f32::from(pixel[3]) / 255.0).max(f32::EPSILON);
            [pixel[2], pixel[1], pixel[0]].map(|channel| (f32::from(channel) / alpha).min(255.0))
        })
        .collect();
    let colors: Vec<Lab> = rgb
        .iter()
        .map(|&[red, green, blue]| oklab(red, green, blue))
        .collect();
    let floor = lightness_floor(&rgb);
    let (cell_width, cell_height) = (sample / GRID_COLUMNS, sample / GRID_ROWS);
    let blocks: Vec<Lab> = (0..GRID_COLUMNS * GRID_ROWS)
        .map(|index| {
            let (column, row) = (index % GRID_COLUMNS, index / GRID_COLUMNS);
            let block = (row * cell_height..(row + 1) * cell_height).flat_map(|y| {
                (column * cell_width..(column + 1) * cell_width).map(move |x| y * sample + x)
            });
            lift(dominant_color(block.map(|pixel| colors[pixel])), floor)
        })
        .collect();
    let size = SPECTRUM_IMAGE_SIZE as usize;
    let position = |value: usize| (value as f32 + 0.5) / size as f32;
    let mut rgba = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let color = srgb(mesh_color(&blocks, position(x), position(y)));
            rgba.extend_from_slice(&[color.r(), color.g(), color.b(), 255]);
        }
    }
    Image::from_rgba8(SPECTRUM_IMAGE_SIZE, SPECTRUM_IMAGE_SIZE, &rgba)
}

fn dominant_color(pixels: impl Iterator<Item = Lab>) -> Lab {
    let mut bins = [(0.0f32, [0.0f32; 3]); HUE_BINS + NEUTRAL_BINS];
    for color in pixels {
        let lch = color.lch();
        let bin = if lch.c < NEUTRAL_CHROMA {
            HUE_BINS + ((lch.l * NEUTRAL_BINS as f32) as usize).min(NEUTRAL_BINS - 1)
        } else {
            let turn = lch.h.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
            ((turn * HUE_BINS as f32) as usize).min(HUE_BINS - 1)
        };
        let weight = 1.0 + lch.c / VIBRANCE_CHROMA;
        let (total, sum) = &mut bins[bin];
        *total += weight;
        sum[0] += color.l * weight;
        sum[1] += color.a * weight;
        sum[2] += color.b * weight;
    }
    let (total, sum) = bins
        .into_iter()
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .unwrap_or_default();
    let total = total.max(f32::EPSILON);
    Lab {
        l: sum[0] / total,
        a: sum[1] / total,
        b: sum[2] / total,
    }
}

fn lightness_floor(rgb: &[[f32; 3]]) -> f32 {
    let count = rgb.len().max(1) as f32;
    let sum = rgb.iter().fold([0.0f32; 3], |sum, pixel| {
        [sum[0] + pixel[0], sum[1] + pixel[1], sum[2] + pixel[2]]
    });
    let [red, green, blue] =
        sum.map(|channel| channel / count * (1.0 - BACKDROP_DIM) + BACKDROP_SHADE * BACKDROP_DIM);
    let backdrop = oklab(red, green, blue).l.powi(3);
    (MIN_CONTRAST * (backdrop + 0.05) - 0.05)
        .cbrt()
        .max(MIN_LIGHTNESS)
}

fn lift(color: Lab, floor: f32) -> Lab {
    Lab {
        l: color.l.max(floor),
        ..color
    }
}

fn mesh_color(blocks: &[Lab], x: f32, y: f32) -> Lab {
    let axis = |value: f32, count: usize| {
        let position = (value * count as f32 - 0.5).clamp(0.0, (count - 1) as f32);
        let index = (position as usize).min(count - 2);
        let fraction = position - index as f32;
        (index, fraction * fraction * (3.0 - 2.0 * fraction))
    };
    let mix = |from: Lab, to: Lab, amount: f32| Lab {
        l: from.l + (to.l - from.l) * amount,
        a: from.a + (to.a - from.a) * amount,
        b: from.b + (to.b - from.b) * amount,
    };
    let ((column, horizontal), (row, vertical)) = (axis(x, GRID_COLUMNS), axis(y, GRID_ROWS));
    let block = |column: usize, row: usize| blocks[row * GRID_COLUMNS + column];
    mix(
        mix(block(column, row), block(column + 1, row), horizontal),
        mix(
            block(column, row + 1),
            block(column + 1, row + 1),
            horizontal,
        ),
        vertical,
    )
}

#[derive(Clone, Copy)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

#[derive(Clone, Copy)]
struct Lch {
    l: f32,
    c: f32,
    h: f32,
}

impl Lab {
    fn lch(self) -> Lch {
        Lch {
            l: self.l,
            c: self.a.hypot(self.b),
            h: self.b.atan2(self.a),
        }
    }
}

impl Lch {
    fn lab(self) -> Lab {
        Lab {
            l: self.l,
            a: self.c * self.h.cos(),
            b: self.c * self.h.sin(),
        }
    }
}

fn srgb_to_linear(channel: f32) -> f32 {
    let channel = channel / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(channel: f32) -> f32 {
    let channel = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (channel * 255.0).clamp(0.0, 255.0)
}

fn oklab(red: f32, green: f32, blue: f32) -> Lab {
    let (r, g, b) = (
        srgb_to_linear(red),
        srgb_to_linear(green),
        srgb_to_linear(blue),
    );
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

fn linear_rgb(color: Lab) -> [f32; 3] {
    let l = (color.l + 0.396_337_78 * color.a + 0.215_803_76 * color.b).powi(3);
    let m = (color.l - 0.105_561_346 * color.a - 0.063_854_17 * color.b).powi(3);
    let s = (color.l - 0.089_484_18 * color.a - 1.291_485_5 * color.b).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

fn srgb(color: Lab) -> Rgba {
    let in_gamut = |rgb: [f32; 3]| rgb.iter().all(|channel| (-0.001..=1.001).contains(channel));
    let mut rgb = linear_rgb(color);
    if !in_gamut(rgb) {
        let lch = color.lch();
        let (mut low, mut high) = (0.0, lch.c);
        for _ in 0..16 {
            let chroma = (low + high) / 2.0;
            if in_gamut(linear_rgb(Lch { c: chroma, ..lch }.lab())) {
                low = chroma;
            } else {
                high = chroma;
            }
        }
        rgb = linear_rgb(Lch { c: low, ..lch }.lab());
    }
    Rgba::from_rgb(
        linear_to_srgb(rgb[0]) as u8,
        linear_to_srgb(rgb[1]) as u8,
        linear_to_srgb(rgb[2]) as u8,
    )
}
