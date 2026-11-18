// Toon (cel) shading for avatars: Unity's avatar shader, DCL_Toon, as the avatar material sets it
// up, in this client's light units. The formulas are
// unity-shared:Runtime/Shaders/Avatar/DCL_Toon/DCL_ToonBodyDoubleShadeWithFeather.hlsl (line
// numbers below are that file's; `unity-shared` is decentraland/unity-shared-dependencies).
//
// - the sun (the brightest directional light) gives a half-Lambert, scaled down to half in its
//   cast shadow, that picks the base colour or one of two grey shades in two feathered steps;
// - the light colour is the sun in light units, saturated, and floored by the ambient light
//   times _Unlit_Intensity, so avatars dim at night but stay visible;
// - a white rim light on the silhouette, in the same light colour.
// No point or spot light reaches the avatar, as in Unity (decisions.md).
//
// The constants are scene_material::toon's, carried as defs by its toon_shader_defs: a float is
// its bit pattern, read with bitcast<f32>. scene_material::toon also holds the CPU twin of the
// functions below, expression for expression.
// Avatars still CAST shadows (that is the shadow pass's job, not this shading).

#import bevy_pbr::{
    pbr_types::PbrInput,
    mesh_view_bindings::{lights, view},
    shadows::fetch_directional_shadow,
    mesh_view_types::DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT,
    mesh_types::MESH_FLAGS_SHADOW_RECEIVER_BIT,
}
#import bevy_render::maths::PI

#ifdef ENVIRONMENT_MAP
#import bevy_pbr::{clustered_forward, clustered_forward::ClusterableObjectIndexRanges, environment_map, lighting}
#endif

// multiplier on emissive wearables so they glow.
const EMISSIVE_BOOST: f32 = 2.0;

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

// the shadow mask (1 in the shades) and the shades term (the first shade blending into the
// second) for a half-Lambert from n_dot_l and the sun's cast-shadow term (1 lit): lines 115 and
// 125-129, with _Set_SystemShadowsToBase 1 and _Tweak_SystemShadowsLevel 0
fn toon_mask_and_shades(n_dot_l: f32, shadow: f32) -> vec2<f32> {
    let base_step = bitcast<f32>(#{TOON_BASE_COLOR_STEP}u);
    let base_feather = bitcast<f32>(#{TOON_BASE_SHADE_FEATHER}u);
    let shade_step = bitcast<f32>(#{TOON_SHADE_COLOR_STEP}u);
    let shades_feather = bitcast<f32>(#{TOON_SHADES_FEATHER}u);

    let half_lambert = 0.5 * n_dot_l + 0.5;
    let shadowed = half_lambert * saturate(shadow * 0.5 + 0.5);
    let mask = saturate(1.0 - (shadowed - (base_step - base_feather)) / base_feather);
    let second = saturate(1.0 - (half_lambert - (shade_step - shades_feather)) / shades_feather);
    let shades = mix(bitcast<f32>(#{TOON_FIRST_SHADE}u), bitcast<f32>(#{TOON_SECOND_SHADE}u), second);
    return vec2(mask, shades);
}

// the lit multiplier on the base colour under the light colour: the shade colours are taken from
// the already lit base colour and lit again (_Use_BaseAs1st, _Is_LightColor_1st_Shade,
// _Is_LightColor_2nd_Shade; lines 105-129)
fn toon_diffuse(n_dot_l: f32, shadow: f32, light: vec3<f32>) -> vec3<f32> {
    let mask_and_shades = toon_mask_and_shades(n_dot_l, shadow);
    return light * mix(vec3(1.0), light * mask_and_shades.y, mask_and_shades.x);
}

// Unity's lightColor (lines 93 and 98, _Is_Filter_LightColor 1): the sun in light units,
// saturated, floored by the ambient light times _Unlit_Intensity and by a minimum
fn toon_light(sun_lux: vec3<f32>, ambient_unity: vec3<f32>) -> vec3<f32> {
    let unlit = bitcast<f32>(#{TOON_UNLIT_INTENSITY}u);
    let unlit_floor = bitcast<f32>(#{TOON_UNLIT_FLOOR}u);
    let default_light = saturate(max(ambient_unity, vec3(unlit_floor)) * unlit);
    return max(default_light, saturate(sun_lux / bitcast<f32>(#{TOON_LIGHT_UNIT_LUX}u)));
}

// Unity's max(ShadeSH9(0), ShadeSH9(down)) (line 93) from the diffuse irradiance of a white
// surface facing up and down: the spherical harmonics' constant term, the mean irradiance over
// every direction, is approximated by the mean of the two
fn toon_ambient(up: vec3<f32>, down: vec3<f32>) -> vec3<f32> {
    return max(0.5 * (up + down), down);
}

#ifdef ENVIRONMENT_MAP
// the diffuse irradiance the pbr path's environment map lookup gives a surface facing `normal`
// at `world_position`: the sky envmap's Trilight ambient, through the top-level shader's
// compute_radiances override (sky_envmap_lookup.wgsl). Only its irradiance is read, so the
// reflection sample is dead code
fn toon_irradiance(normal: vec3<f32>, world_position: vec3<f32>, clusterable_object_index_ranges: ptr<function, ClusterableObjectIndexRanges>) -> vec3<f32> {
    let input = lighting::LayerLightingInput(normal, normal, 1.0, 1.0, 1.0);
    return environment_map::compute_radiances(input, clusterable_object_index_ranges, world_position, false).irradiance;
}
#endif

fn toon_lighting(pbr_input: PbrInput) -> vec4<f32> {
    let base_color = pbr_input.material.base_color;
    // the bands take the vertex normal and the rim the normal-mapped one (_Is_NormalMapToBase 0,
    // _Is_NormalMapToRimLight 1)
    let vertex_n = pbr_input.world_normal;
    let n = pbr_input.N;
    let v = pbr_input.V;
    let world_position = pbr_input.world_position;
    // Unity's light colour 1 in this client's pre-tonemap units: a white pbr surface facing a sun
    // of one light unit
    let unit = bitcast<f32>(#{TOON_LIGHT_UNIT_LUX}u) * view.exposure / PI;

    let view_z = dot(vec4<f32>(
        view.view_from_world[0].z,
        view.view_from_world[1].z,
        view.view_from_world[2].z,
        view.view_from_world[3].z
    ), world_position);

    // the sun is the brightest directional light. Unity keeps a light's direction even when it
    // is black, and with no light at all lights from between the camera's up and back with no
    // colour, leaving the ambient floor (lines 92 and 95)
    var sun_lux = vec3(0.0);
    var sun_luma: f32 = 0.0;
    var sun = 0u;
    var l = normalize(view.world_from_view[1].xyz + view.world_from_view[2].xyz);
    let n_lights = min(lights.n_directional_lights, 4u);
    if n_lights > 0u {
        l = lights.directional_lights[0].direction_to_light;
    }
    for (var i = 0u; i < n_lights; i += 1u) {
        let light = &lights.directional_lights[i];
        let luma = dot((*light).color.rgb, LUMA);
        if luma > sun_luma {
            sun_luma = luma;
            sun = i;
            sun_lux = (*light).color.rgb;
            l = (*light).direction_to_light;
        }
    }

    var shadow: f32 = 1.0;
    if sun_luma > 0.0
        && (lights.directional_lights[sun].flags & DIRECTIONAL_LIGHT_FLAGS_SHADOWS_ENABLED_BIT) != 0u
        && (pbr_input.flags & MESH_FLAGS_SHADOW_RECEIVER_BIT) != 0u {
        shadow = fetch_directional_shadow(sun, world_position, vertex_n, view_z);
    }

    // the ambient light in light units: the diffuse irradiance the pbr path's envmap lookup gives
    // a white surface facing up and down here, before the dielectric factor (0.96) the world's
    // diffuse takes, as Unity's toon reads ShadeSH9 raw; a scene's ambient override and the
    // ambient brightness setting reach avatars as they reach the world. Without the environment
    // map there is none, leaving the floor
    var ambient_unity = vec3(0.0);
#ifdef ENVIRONMENT_MAP
    let cluster_index = clustered_forward::fragment_cluster_index(pbr_input.frag_coord.xy, view_z, pbr_input.is_orthographic);
    var clusterable_object_index_ranges = clustered_forward::unpack_clusterable_object_index_ranges(cluster_index);
    let up = toon_irradiance(vec3(0.0, 1.0, 0.0), world_position.xyz, &clusterable_object_index_ranges);
    let down = toon_irradiance(vec3(0.0, -1.0, 0.0), world_position.xyz, &clusterable_object_index_ranges);
    ambient_unity = toon_ambient(up * view.exposure / unit, down * view.exposure / unit);
#endif
    let light = toon_light(sun_lux, ambient_unity);

    let diffuse = toon_diffuse(dot(vertex_n, l), shadow, light);

    // the rim light (lines 148-157, _RimLight 1, _RimLight_FeatherOff 0, _LightDirection_MaskOn 0,
    // no antipodean rim, white _RimLightColor in the light colour)
    let rim_inside = bitcast<f32>(#{TOON_RIM_INSIDE_MASK}u);
    let rim_area = abs(1.0 - dot(n, v));
    let rim_power = pow(rim_area, exp2(mix(3.0, 0.0, bitcast<f32>(#{TOON_RIM_POWER}u))));
    let rim = saturate((rim_power - rim_inside) / (1.0 - rim_inside))
        * saturate(1.0 + bitcast<f32>(#{TOON_RIM_MASK_LEVEL}u));

    // Unity adds the rim to the lit colours (line 157)
    var color = (base_color.rgb * diffuse + rim * light) * unit;
    color += pbr_input.material.emissive.rgb * view.exposure * EMISSIVE_BOOST;

    return vec4<f32>(color, base_color.a);
}
