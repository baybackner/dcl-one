// The stylized sky, drawn into the atmosphere cube: the sky colour from a lookup by day phase
// and elevation, with a scene's own sky colours blended over it. The lookup follows Unity's
// SkyLut.hlsl; colours are used as authored, with no conversion.

struct StylizedSky {
    tick: u32,
    // seconds
    time: f32,
    // 0 and 1 night, 0.25 sunrise, 0.5 day, 0.75 sunset
    phase: f32,
    moon_active: f32,
    body_direction: vec3<f32>,
    disc_opacity: f32,
    disc_color: vec3<f32>,
    disc_size: f32,
    halo: vec2<f32>,
    cloud_opacity: f32,
    cloud_scroll: f32,
    cloud_lit: vec3<f32>,
    backlight_weight: f32,
    cloud_shadow: vec3<f32>,
    scene_sun_visible: f32,
    // strength, tiling U, tiling V, speed
    horizon_noise: vec4<f32>,
    // scene-authored linear sky colours; alpha flags whether each band is overridden
    scene_zenith: vec4<f32>,
    scene_horizon: vec4<f32>,
    scene_nadir: vec4<f32>,
    scene_rim: vec4<f32>,
    scene_cloud_color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> sky: StylizedSky;
// columns: elevation, the horizon to the zenith. rows: night, sunrise, day, sunset, night;
// filtering between rows blends neighbouring phases
@group(0) @binding(1) var lut_texture: texture_2d<f32>;
@group(0) @binding(2) var lut_sampler: sampler;
@group(0) @binding(3) var noise_texture: texture_2d<f32>;
@group(0) @binding(4) var noise_sampler: sampler;

@group(1) @binding(0)
var image: texture_storage_2d_array<rgba16float, write>;

const TAU: f32 = 6.283185307179586;
const LUT_ROWS: f32 = 5.0;
// degrees of elevation the sky dome's V spans, from the zenith down
const DOME_SPAN_DEG: f32 = 113.6;

// How far the lookup's elevation is pushed up near the horizon by a tiling noise, so the bright
// band shows soft silhouettes of distant clouds. `azimuth` is in turns.
fn horizon_noise(azimuth: f32, elevation_deg: f32) -> f32 {
    let strength = sky.horizon_noise.x;
    let v_dome = (90.0 - elevation_deg) / DOME_SPAN_DEG;
    // only below ~22 degrees of elevation, full at the horizon
    let weight = strength * saturate((v_dome - 0.6) / 0.3);
    if weight <= 0.0 {
        return 0.0;
    }
    // a whole repeat count, so the strip meets itself at the azimuth wrap
    let tiling_u = max(round(sky.horizon_noise.y), 1.0);
    let uv = vec2<f32>(azimuth * tiling_u + sky.time * sky.horizon_noise.w, v_dome * sky.horizon_noise.z);
    // Unity's V runs up the image; a compute shader has no derivatives, so the top level
    return textureSampleLevel(noise_texture, noise_sampler, vec2<f32>(uv.x, 1.0 - uv.y), 0.0).r * weight;
}

// The lookup's colour toward `dir`. Under the horizon the elevation clamps to 0: the horizon
// colour.
fn sky_color(dir: vec3<f32>) -> vec3<f32> {
    // Unity's +Z is -Z here. Straight up and down have no azimuth, and atan2(0, 0) is undefined
    var azimuth = 0.0;
    if dir.x != 0.0 || dir.z != 0.0 {
        azimuth = atan2(dir.x, -dir.z) / TAU;
    }
    let elevation_deg = degrees(asin(clamp(dir.y, -1.0, 1.0)));
    let elevation = saturate(dir.y + horizon_noise(azimuth, elevation_deg));
    let row = sky.phase * (LUT_ROWS - 1.0) / LUT_ROWS + 0.5 / LUT_ROWS;
    return textureSampleLevel(lut_texture, lut_sampler, vec2<f32>(elevation, row), 0.0).rgb;
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) original_invocation_id: vec3<u32>, @builtin(num_workgroups) num_workgroups: vec3<u32>) {
    let size = textureDimensions(image).x;
    let scale = f32(size) / 2f;

    // dither pattern for updates
    let tick = sky.tick & 63;

    var UPDATE_OFFSETS_X: array<u32,64> = array<u32,64>(
        0u, 4u, 4u, 0u, 2u, 6u, 6u, 2u, 2u, 6u, 6u, 2u, 0u, 4u, 4u, 0u,
        1u, 5u, 5u, 1u, 3u, 7u, 7u, 3u, 3u, 7u, 7u, 3u, 1u, 5u, 5u, 1u,
        1u, 5u, 5u, 1u, 3u, 7u, 7u, 3u, 3u, 7u, 7u, 3u, 1u, 5u, 5u, 1u,
        0u, 4u, 4u, 0u, 2u, 6u, 6u, 2u, 2u, 6u, 6u, 2u, 0u, 4u, 4u, 0u
    );

    var UPDATE_OFFSETS_Y: array<u32,64> = array<u32,64>(
        0u, 4u, 0u, 4u, 2u, 6u, 2u, 6u, 0u, 4u, 0u, 4u, 2u, 6u, 2u, 6u,
        1u, 5u, 1u, 5u, 3u, 7u, 3u, 7u, 1u, 5u, 1u, 5u, 3u, 7u, 3u, 7u,
        4u, 0u, 4u, 0u, 6u, 2u, 6u, 2u, 4u, 0u, 4u, 0u, 6u, 2u, 6u, 2u,
        5u, 1u, 5u, 1u, 7u, 3u, 7u, 3u, 5u, 1u, 5u, 1u, 7u, 3u, 7u, 3u
    );

    let invocation_id = original_invocation_id * vec3(8, 8, 1) + vec3(UPDATE_OFFSETS_X[tick], UPDATE_OFFSETS_Y[tick], 0);

    let dir = vec2<f32>((f32(invocation_id.x) / scale) - 1f, (f32(invocation_id.y) / scale) - 1f);

    var ray: vec3<f32>;

    switch invocation_id.z {
        case 0u {
            ray = vec3<f32>(1f, -dir.y, -dir.x); // +X
        }
        case 1u {
            ray = vec3<f32>(-1f, -dir.y, dir.x);// -X
        }
        case 2u {
            ray = vec3<f32>(dir.x, 1f, dir.y); // +Y
        }
        case 3u {
            ray = vec3<f32>(dir.x, -1f, -dir.y);// -Y
        }
        case 4u {
            ray = vec3<f32>(dir.x, -dir.y, 1f); // +Z
        }
        default: {
            ray = vec3<f32>(-dir.x, -dir.y, -1f);// -Z
        }
    }

    ray = normalize(ray);
    var color = sky_color(ray);

    // Each missing band retains the lookup's colour in that direction.
    // Authored colours also feed ambient lighting when an image replaces the visible sky.
    let scene_bands = max(max(sky.scene_zenith.a, sky.scene_horizon.a), max(sky.scene_nadir.a, sky.scene_rim.a));
    if scene_bands > 0.0 {
        let pole_weight = smoothstep(0.0, 1.0, abs(ray.y));
        let pole = select(sky.scene_nadir, sky.scene_zenith, ray.y >= 0.0);
        let horizon_weight = (1.0 - pole_weight) * sky.scene_horizon.a;
        let authored_pole_weight = pole_weight * pole.a;
        color = color * (1.0 - horizon_weight - authored_pole_weight)
            + sky.scene_horizon.rgb * horizon_weight + pole.rgb * authored_pole_weight;
        let rim_weight = (1.0 - smoothstep(0.0, 0.08, abs(ray.y))) * sky.scene_rim.a;
        color = mix(color, sky.scene_rim.rgb, rim_weight);
    }

    // alpha carries the cloud cover, for effects that treat clouds differently from the clear sky
    textureStore(
        image,
        vec2<i32>(invocation_id.xy),
        i32(invocation_id.z),
        vec4<f32>(color, 0.0)
    );
}
