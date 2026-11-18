//! The avatar toon light model, Unity's avatar shader DCL_Toon as the avatar material configures
//! it: its constants, their shader defs for toon.wgsl, and the CPU twin of toon.wgsl's light
//! model, in this client's light units (`common::sky_preset::LIGHT_UNIT_LUX`).
//!
//! Sources (`unity-shared` is decentraland/unity-shared-dependencies, `unity` is
//! decentraland/unity-explorer):
//! - the formulas: `unity-shared:Runtime/Shaders/Avatar/DCL_Toon/DCL_ToonBodyDoubleShadeWithFeather.hlsl`
//!   (line numbers below are that file's), included by `DCL_ToonBody.hlsl`;
//! - the values: `unity:Explorer/Assets/DCL/AvatarRendering/AvatarShape/Assets/Avatar_Toon.mat`,
//!   baked into `unity-shared:Runtime/Shaders/Avatar/DCL_Toon/DCL_ToonVariables.hlsl`.

use bevy::{math::Vec3, render::render_resource::ShaderDefVal};
use common::sky_preset::LIGHT_UNIT_LUX;

/// `_BaseColor_Step` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`): the half-Lambert below
/// which the base colour gives way to the shades
pub const BASE_COLOR_STEP: f32 = 0.2;
/// `_BaseShade_Feather` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`): the width of the
/// base-to-shade transition below the step
pub const BASE_SHADE_FEATHER: f32 = 0.02;
/// `_ShadeColor_Step` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`): the half-Lambert below
/// which the first shade gives way to the second
pub const SHADE_COLOR_STEP: f32 = 0.2;
/// `_1st2nd_Shades_Feather` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`): the width of the
/// first-to-second shade transition
pub const SHADES_FEATHER: f32 = 0.05;
/// `_1st_ShadeColor` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`), a grey
pub const FIRST_SHADE: f32 = 0.9490197;
/// `_2nd_ShadeColor` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`), a grey
pub const SECOND_SHADE: f32 = 0.8000001;
/// `_Unlit_Intensity` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`): the gain on the ambient
/// light that floors the light colour
pub const UNLIT_INTENSITY: f32 = 4.0;
/// The `half3(0.05, 0.05, 0.05)` of `DCL_ToonBodyDoubleShadeWithFeather.hlsl:93`: the ambient
/// floor before `UNLIT_INTENSITY`
pub const UNLIT_FLOOR: f32 = 0.05;
/// `_RimLight_Power` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`)
pub const RIM_POWER: f32 = 0.3;
/// `_RimLight_InsideMask` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`)
pub const RIM_INSIDE_MASK: f32 = 0.15;
/// `_Tweak_RimLightMaskLevel` (`Avatar_Toon.mat`, `DCL_ToonVariables.hlsl`): the rim is scaled
/// by `saturate(1 + RIM_MASK_LEVEL)`
pub const RIM_MASK_LEVEL: f32 = -0.9;

/// The constants above, and `LIGHT_UNIT_LUX` as `TOON_LIGHT_UNIT_LUX`, for the pipeline of every
/// shader that imports toon.wgsl. A def carries only integers, so each `f32` travels as its bit
/// pattern in a `UInt` def and the shader reads it back with `bitcast<f32>(#{NAME}u)`.
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
    ]
    .into_iter()
    .map(|(name, value)| ShaderDefVal::UInt(name.to_owned(), value.to_bits()))
    .collect()
}

/// The shadow mask (1 in the shades) and the shades term (the first shade blending into the
/// second) for a half-Lambert from `n_dot_l` and the sun's cast-shadow term `shadow` (1 lit):
/// lines 115 and 125-129 with `_Set_SystemShadowsToBase` 1, `_Tweak_SystemShadowsLevel` 0 and
/// white shade-position maps. toon.wgsl's `toon_mask_and_shades`.
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

/// The lit multiplier on the base colour under the light colour `light` (`toon_light`), as
/// lines 105-129 compose it: the shade colours are taken from the already lit base colour and
/// lit again (`_Use_BaseAs1st`, `_Is_LightColor_1st_Shade`, `_Is_LightColor_2nd_Shade`), so the
/// shades are `base * light * shade * light`. At a light of 1 this is `toon_band`.
pub fn toon_diffuse(n_dot_l: f32, shadow: f32, light: Vec3) -> Vec3 {
    let (mask, shades) = mask_and_shades(n_dot_l, shadow);
    light * Vec3::ONE.lerp(light * shades, mask)
}

/// Unity's `lightColor` (lines 93 and 98, `_Is_Filter_LightColor` 1):
/// `max(default_light, saturate(sun_lux / LIGHT_UNIT_LUX))` per channel, with the floor
/// `default_light = saturate(max(ambient_unity, UNLIT_FLOOR) * UNLIT_INTENSITY)`. `sun_lux` is
/// the sun's colour times its illuminance; `ambient_unity` is the ambient light in light units
/// (`toon_ambient`).
pub fn toon_light(sun_lux: Vec3, ambient_unity: Vec3) -> Vec3 {
    let default_light = (ambient_unity.max(Vec3::splat(UNLIT_FLOOR)) * UNLIT_INTENSITY)
        .clamp(Vec3::ZERO, Vec3::ONE);
    default_light.max((sun_lux / LIGHT_UNIT_LUX).clamp(Vec3::ZERO, Vec3::ONE))
}

/// Unity's `max(ShadeSH9(0), ShadeSH9(down))` (line 93) from `up` and `down`, the diffuse
/// irradiance of a white surface facing up and down, in light units: the spherical harmonics'
/// constant term, the mean irradiance over every direction, is approximated by the mean of the
/// two.
pub fn toon_ambient(up: Vec3, down: Vec3) -> Vec3 {
    (0.5 * (up + down)).max(down)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).abs().max_element() < 1e-6
    }

    #[test]
    fn bands_follow_unity_steps() {
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
        // the ambient times _Unlit_Intensity lifts the floor
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
