use std::cell::OnceCell;
use std::f32::consts::FRAC_1_SQRT_2;

use skia_safe::{
    Data, FilterMode, Matrix, MipmapMode, Paint, RuntimeEffect, SamplingOptions, TileMode,
    runtime_effect::ChildPtr,
};

use crate::convert::to_skia_rect;
use crate::painter::Painter;
use crate::types::{Rect, Rgba, Vec2};

const CORNER_SMOOTHING_SPAN: f32 = 3.333_333_3;
const CONTINUOUS_CORNER_EXPONENT: f32 = 2.5;
const CIRCULAR_APEX_INSET: f32 = 1.0 - FRAC_1_SQRT_2;
const MAX_BEZEL_FRACTION: f32 = 0.7;
const MAX_DISPERSION: f32 = 0.5;
const EDGE_REACH: f32 = 1.0;
const SHARP_RIM_REACH: f32 = 0.08;

thread_local! {
    static EFFECT: OnceCell<Option<RuntimeEffect>> = const { OnceCell::new() };
}

/// Blurred screen content beneath the current window, addressed in surface pixels.
#[derive(Clone)]
pub struct Backdrop {
    pub(crate) blurred: skia_safe::Image,
    pub(crate) blur_scale: f32,
}

/// Glass silhouette: a continuous rounded rect, optionally fused with a capsule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidShape {
    pub rect: Rect,
    pub radius: f32,
    pub secondary: Option<Rect>,
    pub smoothing: f32,
}

/// Optical, lighting, edge, and shadow parameters; lengths are surface pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiquidMaterial {
    pub bezel_width: f32,
    pub thickness: f32,
    pub glass_ior: f32,
    pub displacement_factor: f32,
    pub dispersion: f32,
    pub morph_dispersion: f32,
    pub tint: Rgba,
    pub legibility: f32,
    pub light: Vec2,
    pub specular_strength: f32,
    pub opposite_specular_strength: f32,
    pub specular_sharpness: f32,
    pub specular_falloff: f32,
    pub specular_opacity: f32,
    pub specular_width: f32,
    pub stroke_width: f32,
    pub stroke_opacity: f32,
    pub stroke_offset_y: f32,
    pub shadow_offset: Vec2,
    pub shadow_radius: f32,
    pub shadow_spread: f32,
    pub shadow_alpha: f32,
    pub black_fade: f32,
}

impl LiquidMaterial {
    pub fn island(scale: f32) -> Self {
        Self {
            bezel_width: 18.0 * scale,
            thickness: 14.0 * scale,
            glass_ior: 1.45,
            displacement_factor: 1.1,
            dispersion: 0.08,
            morph_dispersion: 0.06,
            tint: Rgba::from_argb(95, 10, 11, 15),
            legibility: 0.32,
            light: Vec2::new(-FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
            specular_strength: 0.75,
            opposite_specular_strength: 0.38,
            specular_sharpness: 3.0,
            specular_falloff: 0.9,
            specular_opacity: 0.8,
            specular_width: 1.5 * scale,
            stroke_width: 0.65 * scale,
            stroke_opacity: 0.25,
            stroke_offset_y: 0.2 * scale,
            shadow_offset: Vec2::new(0.0, 2.0 * scale),
            shadow_radius: 7.0 * scale,
            shadow_spread: -scale,
            shadow_alpha: 0.22,
            black_fade: 0.0,
        }
    }

    /// Scales rim displacement by `refraction` and sets the channel split to `dispersion` (0..1).
    pub fn with_optics(mut self, refraction: f32, dispersion: f32) -> Self {
        self.displacement_factor *= refraction.max(0.0);
        self.dispersion = dispersion.clamp(0.0, 1.0) * MAX_DISPERSION;
        self
    }

    /// Scales the dark tint and the adaptive legibility tint by `strength`; 0 leaves clear glass.
    pub fn with_tint(mut self, strength: f32) -> Self {
        let strength = strength.max(0.0);
        let alpha = (f32::from(self.tint.a()) * strength)
            .round()
            .clamp(0.0, 255.0) as u8;
        self.tint = self.tint.with_alpha(alpha);
        self.legibility *= strength;
        self
    }

    pub fn control(scale: f32) -> Self {
        Self {
            bezel_width: 7.0 * scale,
            thickness: 6.0 * scale,
            specular_width: 1.0 * scale,
            stroke_width: 0.5 * scale,
            shadow_radius: 4.0 * scale,
            shadow_offset: Vec2::new(0.0, 1.0 * scale),
            ..Self::island(scale)
        }
    }
}

fn effect() -> Option<RuntimeEffect> {
    EFFECT.with(|cell| {
        cell.get_or_init(|| {
            RuntimeEffect::make_for_shader(include_str!("liquid.sksl"), None)
                .map_err(|error| log::error!("Liquid glass shader compilation failed: {error}"))
                .ok()
        })
        .clone()
    })
}

/// Maps a continuous-corner radius to a superellipse corner whose 45-degree apex matches it.
fn corner(rect: Rect, radius: f32) -> [f32; 2] {
    let limit = (rect.width().min(rect.height()) * 0.5).max(0.5);
    let radius = radius.clamp(0.5, limit);
    let extent =
        radius * CIRCULAR_APEX_INSET / (1.0 - 2.0_f32.powf(-1.0 / CONTINUOUS_CORNER_EXPONENT));
    if extent <= limit {
        return [extent, CONTINUOUS_CORNER_EXPONENT];
    }
    let exponent = (-(1.0 - radius * CIRCULAR_APEX_INSET / limit).log2())
        .recip()
        .clamp(2.0, 2.0 + CORNER_SMOOTHING_SPAN);
    [limit, exponent]
}

fn shape_uniform(rect: Rect) -> [f32; 4] {
    [
        rect.center_x(),
        rect.center_y(),
        rect.width() * 0.5,
        rect.height() * 0.5,
    ]
}

struct Uniforms<'a> {
    effect: &'a RuntimeEffect,
    bytes: Vec<u8>,
}

impl Uniforms<'_> {
    fn set(&mut self, name: &str, values: &[f32]) {
        let Some(uniform) = self.effect.find_uniform(name) else {
            return;
        };
        let offset = uniform.offset();
        for (index, value) in values.iter().enumerate() {
            let start = offset + index * 4;
            if let Some(slot) = self.bytes.get_mut(start..start + 4) {
                slot.copy_from_slice(&value.to_ne_bytes());
            }
        }
    }
}

impl<'a> Painter<'a> {
    /// Draws refractive glass with its edge stroke and shadow over `backdrop`; `progress` runs the
    /// 0..1 material morph.
    /// Returns false if the shader is unavailable.
    pub fn draw_liquid_glass(
        &self,
        backdrop: &Backdrop,
        shape: &LiquidShape,
        material: &LiquidMaterial,
        progress: f32,
        opacity: f32,
    ) -> bool {
        let opacity = opacity.clamp(0.0, 1.0);
        let progress = progress.clamp(0.0, 1.0);
        if opacity <= 0.0
            || progress <= 0.0
            || shape.rect.width() <= 0.0
            || shape.rect.height() <= 0.0
        {
            return true;
        }
        let Some(effect) = effect() else {
            return false;
        };
        let secondary = shape
            .secondary
            .filter(|ball| ball.width() > 0.0 && ball.height() > 0.0);
        let min_half = shape.rect.width().min(shape.rect.height()) * 0.5;
        let bezel = material
            .bezel_width
            .min(min_half * MAX_BEZEL_FRACTION)
            .max(1.0);
        let thickness = material.thickness * bezel / material.bezel_width.max(1.0);
        let light_length = material.light.x.hypot(material.light.y);
        let light = if light_length > 1e-4 {
            [
                material.light.x / light_length,
                material.light.y / light_length,
            ]
        } else {
            [1.0, 0.0]
        };
        let tint_alpha = f32::from(material.tint.a()) / 255.0;

        let mut uniforms = Uniforms {
            effect: &effect,
            bytes: vec![0; effect.uniform_size()],
        };
        uniforms.set("shape0", &shape_uniform(shape.rect));
        uniforms.set("corner0", &corner(shape.rect, shape.radius));
        if let Some(ball) = secondary {
            uniforms.set("shape1", &shape_uniform(ball));
            uniforms.set("corner1", &[ball.width().min(ball.height()) * 0.5, 2.0]);
        }
        uniforms.set("smoothing", &[shape.smoothing.max(0.0)]);
        uniforms.set("bezelWidth", &[bezel]);
        uniforms.set("thickness", &[thickness]);
        uniforms.set("glassIor", &[material.glass_ior]);
        uniforms.set("displacementFactor", &[material.displacement_factor]);
        uniforms.set(
            "dispersion",
            &[material.dispersion.clamp(0.0, MAX_DISPERSION)],
        );
        uniforms.set(
            "morphDispersion",
            &[material.morph_dispersion.clamp(0.0, 0.5)],
        );
        uniforms.set("progress", &[progress]);
        uniforms.set(
            "tint",
            &[
                f32::from(material.tint.r()) / 255.0,
                f32::from(material.tint.g()) / 255.0,
                f32::from(material.tint.b()) / 255.0,
                tint_alpha,
            ],
        );
        uniforms.set("legibility", &[material.legibility.clamp(0.0, 1.0)]);
        uniforms.set("lightDir", &light);
        uniforms.set(
            "specular",
            &[
                material.specular_strength,
                material.opposite_specular_strength,
                material.specular_opacity,
                material.specular_falloff,
            ],
        );
        uniforms.set(
            "specularShape",
            &[material.specular_width, material.specular_sharpness],
        );
        uniforms.set(
            "stroke",
            &[
                material.stroke_width,
                material.stroke_opacity,
                material.stroke_offset_y,
            ],
        );
        uniforms.set(
            "shadow",
            &[
                material.shadow_offset.x,
                material.shadow_offset.y,
                material.shadow_radius,
                material.shadow_spread,
            ],
        );
        uniforms.set("shadowAlpha", &[material.shadow_alpha]);
        uniforms.set("opacity", &[opacity]);
        uniforms.set("blackFade", &[material.black_fade.clamp(0.0, 1.0)]);
        let rim_reach = if backdrop.blur_scale > 1.0 {
            EDGE_REACH
        } else {
            (bezel * SHARP_RIM_REACH * material.displacement_factor).clamp(EDGE_REACH, bezel)
        };
        uniforms.set("rimReach", &[rim_reach]);

        let sampling = SamplingOptions::new(FilterMode::Linear, MipmapMode::None);
        let Some(blurred) = backdrop.blurred.to_shader(
            (TileMode::Clamp, TileMode::Clamp),
            sampling,
            &Matrix::scale((backdrop.blur_scale, backdrop.blur_scale)),
        ) else {
            return false;
        };
        let children = [ChildPtr::from(blurred)];
        let Some(shader) = effect.make_shader(Data::new_copy(&uniforms.bytes), &children, None)
        else {
            return false;
        };

        let bounds = secondary.map_or(shape.rect, |ball| {
            Rect::from_ltrb(
                shape.rect.left.min(ball.left),
                shape.rect.top.min(ball.top),
                shape.rect.right.max(ball.right),
                shape.rect.bottom.max(ball.bottom),
            )
        });
        let margin = (material.shadow_radius + material.shadow_spread.max(0.0))
            .max(material.stroke_width + material.stroke_offset_y)
            + material
                .shadow_offset
                .x
                .abs()
                .max(material.shadow_offset.y.abs())
            + 2.0;
        let mut paint = Paint::default();
        paint.set_shader(shader);
        self.canvas().draw_rect(
            to_skia_rect(Rect::from_ltrb(
                bounds.left - margin,
                bounds.top - margin,
                bounds.right + margin,
                bounds.bottom + margin,
            )),
            &paint,
        );
        true
    }
}
