//! The avatar toon light model's constants and their shader defs for toon.wgsl and the outline in
//! bound_material_effect.wgsl, in this client's light units (`common::sky_preset::LIGHT_UNIT_LUX`).
//! The sun's half-Lambert picks the base colour or one of two grey shades in two feathered steps,
//! under a light colour that is the sun in light units, saturated, and floored by the ambient
//! light so avatars stay visible at night: the sky environment map's where the view has it, the
//! view's flat ambient light where it has not. The outline is a tone of the surface it borders,
//! thins with the avatar's distance and, in a perspective view, stops at 8 m.

use bevy::{math::Vec3, render::render_resource::ShaderDefVal};
use common::sky_preset::{ambient_trilight, LIGHT_UNIT_LUX};

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
/// The gain on the ambient light that lights the eyes, eyebrows and mouth
pub const FACIAL_AMBIENT_GAIN: f32 = 4.0;
/// The ambient light, in light units, that lights the eyes, eyebrows and mouth in a view without
/// the sky environment map (the Backpack preview, a photo booth): the noon Trilight's equator
/// colour, about what the sky gives a surface facing the horizon at noon. Such a view's flat
/// ambient is set to light the body flat, far above any sky, so it is not read here.
pub fn facial_flat_ambient() -> Vec3 {
    ambient_trilight(0.5)[1]
}

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
        ("TOON_FACIAL_AMBIENT_GAIN", FACIAL_AMBIENT_GAIN),
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
    .chain({
        let flat = facial_flat_ambient();
        [
            ("TOON_FACIAL_FLAT_AMBIENT_R", flat.x),
            ("TOON_FACIAL_FLAT_AMBIENT_G", flat.y),
            ("TOON_FACIAL_FLAT_AMBIENT_B", flat.z),
        ]
    })
    .map(|(name, value)| ShaderDefVal::UInt(name.to_owned(), value.to_bits()))
    .collect()
}
