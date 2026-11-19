// The stylized sky, drawn into the atmosphere cube: the sky colour from a lookup by day phase
// and elevation, with a scene's own sky colours blended over it, under three strips of cloud.
// The lookup follows Unity's SkyLut.hlsl and the clouds its CloudsV2.hlsl; colours are used as
// authored, with no conversion.

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
    // scales every cloud strip's opacity
    cloud_opacity: f32,
    // how much the body behind a cloud lights it through
    backlight_weight: f32,
    cloud_lit: vec3<f32>,
    scene_sun_visible: f32,
    cloud_shadow: vec3<f32>,
    // strength, tiling U, tiling V, speed
    horizon_noise: vec4<f32>,
    // scene-authored linear sky colours; alpha flags whether each band is overridden
    scene_zenith: vec4<f32>,
    scene_horizon: vec4<f32>,
    scene_nadir: vec4<f32>,
    scene_rim: vec4<f32>,
    // ramp knee, highlight strength, highlight threshold, highlight falloff
    cloud_params: vec4<f32>,
    // zenith fade start and end, occlusion start and end
    cloud_fades: vec4<f32>,
    // per strip: stretch and offset down the dome, repeats and offset around it
    cloud_layers: array<vec4<f32>, 3>,
    // per strip: strength, opacity, flow at the phase, scroll in turns
    cloud_looks: array<vec4<f32>, 3>,
}

@group(0) @binding(0) var<uniform> sky: StylizedSky;
// columns: elevation, the horizon to the zenith. rows: night, sunrise, day, sunset, night;
// filtering between rows blends neighbouring phases
@group(0) @binding(1) var lut_texture: texture_2d<f32>;
@group(0) @binding(2) var lut_sampler: sampler;
@group(0) @binding(3) var noise_texture: texture_2d<f32>;
@group(0) @binding(4) var noise_sampler: sampler;
// one layer a strip, back to front: each a turn of the sky wide, repeating around it. R the
// shaded cloud, G its backlit look, B the growth order, A the mask
@group(0) @binding(5) var cloud_texture: texture_2d_array<f32>;
@group(0) @binding(6) var cloud_sampler: sampler;

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
// colour. `azimuth` is in turns.
fn sky_color(dir: vec3<f32>, azimuth: f32, elevation_deg: f32) -> vec3<f32> {
    let elevation = saturate(dir.y + horizon_noise(azimuth, elevation_deg));
    let row = sky.phase * (LUT_ROWS - 1.0) / LUT_ROWS + 0.5 / LUT_ROWS;
    return textureSampleLevel(lut_texture, lut_sampler, vec2<f32>(elevation, row), 0.0).rgb;
}

// A cloud texel's colour between the shadowed and the lit colour, through a midtone at the knee.
fn cloud_ramp(t: f32) -> vec3<f32> {
    let knee = sky.cloud_params.x;
    let mid = mix(sky.cloud_shadow, sky.cloud_lit, 0.35);
    if t < knee {
        return mix(sky.cloud_shadow, mid, saturate(t / max(knee, 1e-3)));
    }
    return mix(mid, sky.cloud_lit, saturate((t - knee) / max(1.0 - knee, 1e-3)));
}

// The cloud strips toward `dir`, back to front: their colour, premultiplied, and their alpha.
// `azimuth` is in turns.
fn clouds(dir: vec3<f32>, azimuth: f32, elevation_deg: f32) -> vec4<f32> {
    // each strip hangs on a band of the sky dome, whose V is 0 at the zenith and 1 at its rim
    // under the horizon
    let v_dome = (90.0 - elevation_deg) / DOME_SPAN_DEG;

    // the body behind a cloud drives its shading toward the backlit look and adds a rim; only
    // the drawn body's own side lights
    let threshold = sky.cloud_params.z;
    let facing = saturate((dot(dir, sky.body_direction) - threshold) / max(1.0 - threshold, 1e-3));
    let backlight = pow(facing, max(sky.cloud_params.w, 1e-3)) * sky.backlight_weight;

    // into the horizon haze, hidden below the horizon, and out toward the zenith, where the
    // strips pinch. The zenith edges are kept apart: smoothstep with equal edges divides by zero
    let zenith_fade = 1.0 - smoothstep(sky.cloud_fades.x, max(sky.cloud_fades.y, sky.cloud_fades.x + 1e-4), dir.y);
    let fade = smoothstep(-0.35, -0.05, dir.y) * zenith_fade * sky.cloud_opacity;

    var color = vec3<f32>(0.0);
    var alpha = 0.0;
    for (var index = 0; index < 3; index++) {
        let layer = sky.cloud_layers[index];
        let look = sky.cloud_looks[index];
        let v_tex = (v_dome + layer.y - 0.5) / max(layer.x, 1e-3) + 0.5;
        let uv = vec2<f32>(azimuth * layer.z + layer.w - look.w, saturate(1.0 - v_tex));
        // Unity's V runs up the image; a compute shader has no derivatives, so the top level
        let strip = textureSampleLevel(cloud_texture, cloud_sampler, vec2<f32>(uv.x, 1.0 - uv.y), index, 0.0);

        // the strips fade to black where the mask fades; undone, soft edges keep the cloud's
        // colour and draw no dark outline
        let shade = saturate(strip.rgb / max(strip.a, 1e-3));
        var layer_color = cloud_ramp(mix(shade.r, shade.g, backlight)) * look.x;
        layer_color += shade.g * backlight * sky.cloud_params.y * sky.cloud_lit;

        // texels whose growth order is under the flow are there
        let layer_alpha = strip.a * saturate((look.z * 1.3 - shade.b) / 0.3) * fade * look.y;

        color = color * (1.0 - layer_alpha) + layer_color * layer_alpha;
        alpha = alpha + layer_alpha * (1.0 - alpha);
    }
    return vec4<f32>(color, alpha);
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
    // Unity's +Z is -Z here. Straight up and down have no azimuth, and atan2(0, 0) is undefined
    var azimuth = 0.0;
    if ray.x != 0.0 || ray.z != 0.0 {
        azimuth = atan2(ray.x, -ray.z) / TAU;
    }
    let elevation_deg = degrees(asin(clamp(ray.y, -1.0, 1.0)));
    var color = sky_color(ray, azimuth, elevation_deg);

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

    // the clouds over the sky. Alpha is how much they hide of what lies behind them, the stars
    // and the sun: only mostly opaque cloud does, or soft edges would wear a dark ring. The
    // edges are kept apart, as above
    let cloud = clouds(ray, azimuth, elevation_deg);
    color = color * (1.0 - cloud.a) + cloud.rgb;
    let occlusion = smoothstep(sky.cloud_fades.z, max(sky.cloud_fades.w, sky.cloud_fades.z + 1e-4), cloud.a);

    textureStore(
        image,
        vec2<i32>(invocation_id.xy),
        i32(invocation_id.z),
        vec4<f32>(color, occlusion)
    );
}
