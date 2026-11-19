//! The avatar toon light model: its constants, their shader defs for toon.wgsl, and the CPU twin
//! of toon.wgsl's light model, in this client's light units (`common::sky_preset::LIGHT_UNIT_LUX`).
//! The sun's half-Lambert picks the base colour or one of two grey shades in two feathered steps,
//! under a light colour that is the sun in light units, saturated, and floored by the ambient
//! light so avatars stay visible at night: the sky environment map's where the view has it, the
//! view's flat ambient light where it has not. The outline is a tone of the surface it borders,
//! thins with the avatar's distance and, in a perspective view, stops at 8 m.

use std::f32::consts::PI;

use bevy::{math::Vec3, render::render_resource::ShaderDefVal};
use common::sky_preset::LIGHT_UNIT_LUX;

/// The half-Lambert below which the base colour gives way to the shades
pub const BASE_COLOR_STEP: f32 = 0.2;
/// The width of the base-to-shade transition below `BASE_COLOR_STEP`
pub const BASE_SHADE_FEATHER: f32 = 0.02;
/// The half-Lambert below which the first shade gives way to the second
pub const SHADE_COLOR_STEP: f32 = 0.2;
/// The width of the first-to-second shade transition below `SHADE_COLOR_STEP`
pub const SHADES_FEATHER: f32 = 0.05;
/// The grey the lit base colour is multiplied by in the first shade
pub const FIRST_SHADE: f32 = 0.9490197;
/// The grey of the second, darker shade
pub const SECOND_SHADE: f32 = 0.8000001;
/// The gain on the ambient light that floors the light colour
pub const UNLIT_INTENSITY: f32 = 4.0;
/// The least ambient light the floor takes, before `UNLIT_INTENSITY`: with no light at all an
/// avatar is lit at a fifth of noon
pub const UNLIT_FLOOR: f32 = 0.05;
/// The rim light's width: the rim is the silhouette term to the power `2^(3 - 3 * RIM_POWER)`, so
/// 0 keeps it at the very edge and 1 spreads it over the body
pub const RIM_POWER: f32 = 0.3;
/// The silhouette term below which there is no rim light; above it the rim rises to 1
pub const RIM_INSIDE_MASK: f32 = 0.15;
/// The rim light's strength, as `saturate(1 + RIM_MASK_LEVEL)`: a tenth
pub const RIM_MASK_LEVEL: f32 = -0.9;

/// The outline's width in metres at and below `OUTLINE_NEAREST_M` from the camera
pub const OUTLINE_WIDTH_M: f32 = 0.002;
/// The camera distance at and below which the outline has its full width
pub const OUTLINE_NEAREST_M: f32 = 0.5;
/// The camera distance at and beyond which the outline has no width
pub const OUTLINE_FARTHEST_M: f32 = 100.0;
/// The grey of the outline, taken times the base colour twice: a darker, more saturated tone of
/// the surface it borders, not lit
pub const OUTLINE_COLOR: f32 = 0.6320754;
/// The squared camera distance beyond which an avatar has no outline in a perspective view: 8 m
pub const OUTLINE_MAX_SQR_DISTANCE: f32 = 64.0;
/// How far behind a fragment the mean of its four diagonal neighbours must lie for the line, in
/// pixel footprints (the metres one step to the neighbours spans at the fragment's depth). A
/// straight edge has half its neighbours behind it, so the surface behind needs a gap of three
/// footprints: a chin over the neck, an ear or a lock of hair is lined, as an inverted hull lines
/// any overlap, while a curved surface's own rise towards its silhouette stays below it.
pub const OUTLINE_EDGE_PX: f32 = 1.5;

/// The constants above, and `LIGHT_UNIT_LUX` as `TOON_LIGHT_UNIT_LUX`, for the pipeline of every
/// shader that imports toon.wgsl or bound_material_effect.wgsl's `toon_outline`. A def carries
/// only integers, so each `f32` travels as its bit pattern in a `UInt` def and the shader reads
/// it back with `bitcast<f32>(#{NAME}u)`.
pub fn toon_shader_defs() -> Vec<ShaderDefVal> {
    [
        ("TOON_BASE_COLOR_STEP", BASE_COLOR_STEP),
        ("TOON_BASE_SHADE_FEATHER", BASE_SHADE_FEATHER),
        ("TOON_SHADE_COLOR_STEP", SHADE_COLOR_STEP),
        ("TOON_SHADES_FEATHER", SHADES_FEATHER),
        ("TOON_FIRST_SHADE", FIRST_SHADE),
        ("TOON_SECOND_SHADE", SECOND_SHADE),
        ("TOON_UNLIT_INTENSITY", UNLIT_INTENSITY),
        ("TOON_UNLIT_FLOOR", UNLIT_FLOOR),
        ("TOON_RIM_POWER", RIM_POWER),
        ("TOON_RIM_INSIDE_MASK", RIM_INSIDE_MASK),
        ("TOON_RIM_MASK_LEVEL", RIM_MASK_LEVEL),
        ("TOON_LIGHT_UNIT_LUX", LIGHT_UNIT_LUX),
        ("TOON_OUTLINE_WIDTH_M", OUTLINE_WIDTH_M),
        ("TOON_OUTLINE_NEAREST_M", OUTLINE_NEAREST_M),
        ("TOON_OUTLINE_FARTHEST_M", OUTLINE_FARTHEST_M),
        ("TOON_OUTLINE_COLOR", OUTLINE_COLOR),
        ("TOON_OUTLINE_MAX_SQR_DISTANCE", OUTLINE_MAX_SQR_DISTANCE),
        ("TOON_OUTLINE_EDGE_PX", OUTLINE_EDGE_PX),
    ]
    .into_iter()
    .map(|(name, value)| ShaderDefVal::UInt(name.to_owned(), value.to_bits()))
    .collect()
}

/// The shadow mask (1 in the shades) and the shades term (the first shade blending into the
/// second) for a half-Lambert from `n_dot_l` and the sun's cast-shadow term `shadow` (1 lit). A
/// full shadow halves the half-Lambert the mask reads, so only surfaces already turning away from
/// the sun fall into the shades; the shades blend on the unshadowed half-Lambert. toon.wgsl's
/// `toon_mask_and_shades`.
fn mask_and_shades(n_dot_l: f32, shadow: f32) -> (f32, f32) {
    let half_lambert = 0.5 * n_dot_l + 0.5;
    let shadowed = half_lambert * (shadow * 0.5 + 0.5).clamp(0.0, 1.0);
    let mask = (1.0 - (shadowed - (BASE_COLOR_STEP - BASE_SHADE_FEATHER)) / BASE_SHADE_FEATHER)
        .clamp(0.0, 1.0);
    let second = (1.0 - (half_lambert - (SHADE_COLOR_STEP - SHADES_FEATHER)) / SHADES_FEATHER)
        .clamp(0.0, 1.0);
    let shades = FIRST_SHADE + (SECOND_SHADE - FIRST_SHADE) * second;
    (mask, shades)
}

/// The multiplier on the base colour under a white light of 1: 1, `FIRST_SHADE` or
/// `SECOND_SHADE`, feathered, from the half-Lambert with the cast shadow folded in.
pub fn toon_band(n_dot_l: f32, shadow: f32) -> f32 {
    let (mask, shades) = mask_and_shades(n_dot_l, shadow);
    1.0 + (shades - 1.0) * mask
}

/// The lit multiplier on the base colour under the light colour `light` (`toon_light`): the
/// shade colours are taken from the already lit base colour and lit again, so the shades are
/// `base * light * shade * light` and darken faster than the light dims. At a light of 1 this is
/// `toon_band`.
pub fn toon_diffuse(n_dot_l: f32, shadow: f32, light: Vec3) -> Vec3 {
    let (mask, shades) = mask_and_shades(n_dot_l, shadow);
    light * Vec3::ONE.lerp(light * shades, mask)
}

/// The light colour: `max(default_light, saturate(sun_lux / LIGHT_UNIT_LUX))` per channel, with
/// the floor `default_light = saturate(max(ambient, UNLIT_FLOOR) * UNLIT_INTENSITY)`.
/// Saturated so a bright sun never lifts avatars past white, floored so they stay visible at
/// night. `sun_lux` is the sun's colour times its illuminance; `ambient` is the ambient light in
/// light units (`toon_ambient` with the sky environment map, `toon_flat_ambient` without).
pub fn toon_light(sun_lux: Vec3, ambient: Vec3) -> Vec3 {
    let default_light =
        (ambient.max(Vec3::splat(UNLIT_FLOOR)) * UNLIT_INTENSITY).clamp(Vec3::ZERO, Vec3::ONE);
    default_light.max((sun_lux / LIGHT_UNIT_LUX).clamp(Vec3::ZERO, Vec3::ONE))
}

/// The ambient light that floors `toon_light`, from `up` and `down`, the diffuse irradiance of a
/// white surface facing up and down, in light units: the mean irradiance over every direction,
/// approximated by the mean of the two, or the light from below where that is brighter.
pub fn toon_ambient(up: Vec3, down: Vec3) -> Vec3 {
    (0.5 * (up + down)).max(down)
}

/// The ambient light of a view without the sky environment map, in light units, from its flat
/// ambient light `ambient_color`: the `AmbientLight`'s linear colour times its brightness, as the
/// lights uniform carries it. The pbr path lights a white surface by the flat ambient at that
/// value, and by a sun facing it at its illuminance over pi, so one light unit of flat ambient is
/// `LIGHT_UNIT_LUX / PI`.
pub fn toon_flat_ambient(ambient_color: Vec3) -> Vec3 {
    ambient_color.max(Vec3::ZERO) * PI / LIGHT_UNIT_LUX
}

/// The outline's width in pixels on a fragment `view_z` metres deep, for an avatar
/// `camera_distance` metres from the camera and a vertical focal length of `focal_px` pixels:
/// `OUTLINE_WIDTH_M` thinning smoothly from `OUTLINE_NEAREST_M` to nothing at
/// `OUTLINE_FARTHEST_M`, projected at the fragment's depth. bound_material_effect.wgsl's
/// `toon_outline`.
pub fn outline_width_px(camera_distance: f32, view_z: f32, focal_px: f32) -> f32 {
    let t = ((camera_distance - OUTLINE_NEAREST_M) / (OUTLINE_FARTHEST_M - OUTLINE_NEAREST_M))
        .clamp(0.0, 1.0);
    OUTLINE_WIDTH_M * (1.0 - t * t * (3.0 - 2.0 * t)) * focal_px / view_z
}

/// The depth, in metres, by which the mean of a fragment's diagonal neighbours `step_px` pixels
/// away must lie behind it for the line, on a fragment `view_z` metres deep under a vertical focal
/// length of `focal_px` pixels (an orthographic view's pixels per metre, at any depth).
/// bound_material_effect.wgsl's `apply_outline` with `OUTLINE_EDGE_PX`.
pub fn outline_edge_depth_m(step_px: f32, view_z: f32, focal_px: f32, orthographic: bool) -> f32 {
    let pixel_m = if orthographic {
        1.0 / focal_px
    } else {
        view_z / focal_px
    };
    OUTLINE_EDGE_PX * step_px * pixel_m
}

/// Whether a mesh whose origin is `sqr_distance` square metres from the camera has an outline.
/// An orthographic view has no cutoff: its camera stands back however large it draws the avatar.
pub fn outline_visible(sqr_distance: f32, orthographic: bool) -> bool {
    orthographic || sqr_distance <= OUTLINE_MAX_SQR_DISTANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-6
    }

    #[test]
    fn bands_follow_the_steps() {
        // lit, half-Lambert 1
        assert!((toon_band(1.0, 1.0) - 1.0).abs() < 1e-6);
        // half-Lambert 0: below both steps
        assert!((toon_band(-1.0, 1.0) - SECOND_SHADE).abs() < 1e-6);
        // half-Lambert 0.19 out of shadow: inside the base feather, above the shades' one
        let band = toon_band((0.19 - 0.5) * 2.0, 1.0);
        assert!(band < 1.0 && band > SECOND_SHADE);
        // a full cast shadow halves the half-Lambert: 1 halves to 0.5, still above the base step
        assert_eq!(toon_band(1.0, 0.0), 1.0);
        // half-Lambert 0.3 is lit out of shadow; in shadow it halves to 0.15, below the base
        // step's feather, and the shades at 0.3 are the first shade
        assert_eq!(toon_band(-0.4, 1.0), 1.0);
        assert!((toon_band(-0.4, 0.0) - FIRST_SHADE).abs() < 1e-6);
        // beyond the shadow distance, or with shadows off, the shadow term is 1: the lit band
        assert_eq!(toon_band(0.8, 1.0), 1.0);
    }

    #[test]
    fn shades_take_the_light_twice() {
        // the lit band is base * light; the shades are base * light * shade * light
        let night = Vec3::splat(UNLIT_FLOOR * UNLIT_INTENSITY);
        assert_eq!(toon_diffuse(1.0, 1.0, night), night);
        assert!(close(
            toon_diffuse(-1.0, 1.0, night),
            night * night * SECOND_SHADE
        ));
        // at full light the composition is the band
        for (n_dot_l, shadow) in [(1.0, 1.0), (-1.0, 1.0), (-0.62, 1.0), (-0.4, 0.0)] {
            assert!(close(
                toon_diffuse(n_dot_l, shadow, Vec3::ONE),
                Vec3::splat(toon_band(n_dot_l, shadow))
            ));
        }
    }

    #[test]
    fn light_saturates_and_floors() {
        // noon is 1.2 light units, saturated to 1
        assert_eq!(
            toon_light(Vec3::splat(1.2 * LIGHT_UNIT_LUX), Vec3::ZERO),
            Vec3::ONE
        );
        // a huge scene sun never exceeds noon
        assert_eq!(toon_light(Vec3::splat(1e9), Vec3::ZERO), Vec3::ONE);
        // a black sun and no ambient: the 0.05 * 4 floor
        assert_eq!(toon_light(Vec3::ZERO, Vec3::ZERO), Vec3::splat(0.2));
        // the ambient times the gain lifts the floor
        assert!(close(
            toon_light(Vec3::ZERO, Vec3::splat(0.1)),
            Vec3::splat(0.4)
        ));
        // between the floor and noon, the sun in light units
        assert!(close(
            toon_light(Vec3::splat(0.3 * LIGHT_UNIT_LUX), Vec3::ZERO),
            Vec3::splat(0.3)
        ));
    }

    #[test]
    fn flat_ambient_lights_a_view_without_the_sky_flat() {
        // one light unit of flat ambient lights a white surface as a sun of one light unit does
        assert!(close(
            toon_flat_ambient(Vec3::splat(LIGHT_UNIT_LUX / PI)),
            Vec3::ONE
        ));
        // a brightness of 5000 (an override of 5 at the default ambient setting of 50, times
        // 20) is 5 light units and a tinted ambient keeps its tint
        assert!(close(
            toon_flat_ambient(Vec3::new(5000.0, 2500.0, 0.0)),
            Vec3::new(5000.0, 2500.0, 0.0) * PI / LIGHT_UNIT_LUX
        ));
        assert!((toon_flat_ambient(Vec3::splat(5000.0)).x - 5.009_469).abs() < 1e-4);
        // which saturates the floor: the avatar is lit flat at 1 with no sun at all
        assert_eq!(
            toon_light(Vec3::ZERO, toon_flat_ambient(Vec3::splat(5000.0))),
            Vec3::ONE
        );
        // a dim flat ambient lifts the floor by the gain
        assert!(close(
            toon_light(
                Vec3::ZERO,
                toon_flat_ambient(Vec3::splat(0.1 * LIGHT_UNIT_LUX / PI))
            ),
            Vec3::splat(0.4)
        ));
        // none leaves the minimum, and a negative value counts as none
        assert_eq!(
            toon_light(Vec3::ZERO, toon_flat_ambient(Vec3::ZERO)),
            Vec3::splat(0.2)
        );
        assert_eq!(toon_flat_ambient(Vec3::splat(-1.0)), Vec3::ZERO);
    }

    #[test]
    fn ambient_takes_the_brighter_of_average_and_down() {
        assert!(close(
            toon_ambient(Vec3::splat(0.4), Vec3::splat(0.2)),
            Vec3::splat(0.3)
        ));
        assert!(close(
            toon_ambient(Vec3::ZERO, Vec3::splat(0.2)),
            Vec3::splat(0.2)
        ));
    }

    #[test]
    fn outline_width_shrinks_with_distance_and_cuts_at_eight_metres() {
        // 1080p at 60 degrees vertical
        let focal = 540.0 / (30f32.to_radians()).tan();
        // 1 m is 0.5 m into the 99.5 m falloff
        let t = 0.5 / 99.5;
        let at_one_metre = 0.002 * (1.0 - t * t * (3.0 - 2.0 * t)) * focal;
        assert!((outline_width_px(1.0, 1.0, focal) - at_one_metre).abs() < 1e-6);
        assert!((at_one_metre - 0.002 * focal).abs() < 0.05);
        // a fragment twice as deep has half the width
        assert!((outline_width_px(1.0, 2.0, focal) - 0.5 * at_one_metre).abs() < 1e-6);
        // full width at and inside the nearest distance, none at and past the farthest
        assert_eq!(outline_width_px(0.5, 1.0, focal), 0.002 * focal);
        assert_eq!(outline_width_px(0.1, 1.0, focal), 0.002 * focal);
        assert_eq!(outline_width_px(100.0, 100.0, focal), 0.0);
        assert_eq!(outline_width_px(150.0, 150.0, focal), 0.0);
        assert!(outline_width_px(8.0, 8.0, focal) < 0.3);
        assert!(outline_visible(64.0, false) && !outline_visible(64.0001, false));
        // an orthographic view outlines at any distance, at the width its distance gives
        assert!(outline_visible(64.0001, true) && outline_visible(1e6, true));
        assert!(outline_width_px(8.5, 1.0, focal) > 0.98 * 0.002 * focal);
    }

    #[test]
    fn edges_line_overlaps_of_a_few_pixels() {
        // 1080p at 60 degrees vertical, an avatar 2 m away: a pixel spans about 2 mm
        let focal = 540.0 / (30f32.to_radians()).tan();
        let pixel_m = 2.0 / focal;
        let mean = outline_edge_depth_m(1.0, 2.0, focal, false);
        assert!((mean - 1.5 * pixel_m).abs() < 1e-7);
        // a straight edge needs twice that: a chin a few centimetres before the neck is lined
        assert!(2.0 * mean < 0.01);
        // the depth a 15 cm curve gains one footprint short of its silhouette stays below it:
        // its second difference over the steps half, one and a half and two and a half
        // footprints in is about 0.08 * sqrt(2 * r * footprint)
        let curve = 0.0807 * (2.0 * 0.15 * pixel_m).sqrt();
        assert!(curve < mean);
        // the footprint grows with the depth and the step
        assert!((outline_edge_depth_m(2.0, 4.0, focal, false) - 4.0 * mean).abs() < 1e-7);
        // an orthographic view has one footprint at every depth
        assert_eq!(
            outline_edge_depth_m(1.0, 2.0, 100.0, true),
            outline_edge_depth_m(1.0, 20.0, 100.0, true)
        );
        assert!((outline_edge_depth_m(1.0, 20.0, 100.0, true) - 0.015).abs() < 1e-7);
    }

    #[test]
    fn wgsl_defs_mirror_the_constants() {
        let expected = [
            ("TOON_BASE_COLOR_STEP", BASE_COLOR_STEP),
            ("TOON_BASE_SHADE_FEATHER", BASE_SHADE_FEATHER),
            ("TOON_SHADE_COLOR_STEP", SHADE_COLOR_STEP),
            ("TOON_SHADES_FEATHER", SHADES_FEATHER),
            ("TOON_FIRST_SHADE", FIRST_SHADE),
            ("TOON_SECOND_SHADE", SECOND_SHADE),
            ("TOON_UNLIT_INTENSITY", UNLIT_INTENSITY),
            ("TOON_UNLIT_FLOOR", UNLIT_FLOOR),
            ("TOON_RIM_POWER", RIM_POWER),
            ("TOON_RIM_INSIDE_MASK", RIM_INSIDE_MASK),
            ("TOON_RIM_MASK_LEVEL", RIM_MASK_LEVEL),
            ("TOON_LIGHT_UNIT_LUX", LIGHT_UNIT_LUX),
            ("TOON_OUTLINE_WIDTH_M", OUTLINE_WIDTH_M),
            ("TOON_OUTLINE_NEAREST_M", OUTLINE_NEAREST_M),
            ("TOON_OUTLINE_FARTHEST_M", OUTLINE_FARTHEST_M),
            ("TOON_OUTLINE_COLOR", OUTLINE_COLOR),
            ("TOON_OUTLINE_MAX_SQR_DISTANCE", OUTLINE_MAX_SQR_DISTANCE),
            ("TOON_OUTLINE_EDGE_PX", OUTLINE_EDGE_PX),
        ];
        let defs = toon_shader_defs();
        assert_eq!(defs.len(), expected.len());
        for (name, value) in expected {
            let bits = defs.iter().find_map(|def| match def {
                ShaderDefVal::UInt(def_name, bits) if def_name == name => Some(*bits),
                _ => None,
            });
            assert_eq!(bits.map(f32::from_bits), Some(value), "{name}");
        }
    }
}
