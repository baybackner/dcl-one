//! The sky: Unity's stylized skybox (the StylizedV1 preset), drawn into the atmosphere cube by
//! `shaders/stylized_sky.wgsl`. The sky colour is a lookup by day phase and elevation, baked
//! from the preset's four gradients (`sky_preset::bake_sky_lut`); a scene's own sky colours
//! blend over it.
//!
//! The authored colours go to the shader as they are, and the skybox material writes the cube
//! to the view unscaled, so the cube is in the units of Unity's sky output.

use bevy::{
    image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::{
        render_asset::{RenderAssetUsages, RenderAssets},
        render_resource::{
            binding_types::{sampler, texture_2d, texture_2d_array, uniform_buffer},
            encase, BindGroup, BindGroupEntries, BindGroupLayout, BindGroupLayoutEntries,
            BufferInitDescriptor, BufferUsages, ComputePipelineDescriptor, Extent3d, PipelineCache,
            SamplerBindingType, ShaderStages, TextureDimension, TextureFormat, TextureSampleType,
        },
        renderer::RenderDevice,
        texture::{FallbackImage, GpuImage},
        RenderApp,
    },
};
use bevy_atmosphere::{model::AtmosphereModelMetadata, pipeline::AtmosphereImageBindGroupLayout};

use crate::sky_preset::{bake_sky_lut, HORIZON_NOISE, SKY_LUT_ROWS, SKY_LUT_WIDTH};

const SHADER_PATH: &str = "embedded://shaders/stylized_sky.wgsl";
const HORIZON_NOISE_PATH: &str = "embedded://sky/horizon_noise.png";

/// The stylized sky model. `apply_global_light` writes it every frame.
#[derive(Reflect, Debug, Clone)]
pub struct StylizedSky {
    /// Frame counter: each frame redraws one texel in 64 of the cube, in a dither pattern.
    pub tick: u32,
    /// Seconds, driving the scrolls.
    pub time: f32,
    /// The day phase: 0 and 1 night, 0.25 sunrise, 0.5 day, 0.75 sunset.
    pub phase: f32,
    /// World direction toward the drawn disc, the sun or the moon.
    pub body_direction: Vec3,
    /// 1 while the disc is the moon.
    pub moon_active: f32,
    pub disc_opacity: f32,
    pub disc_size: f32,
    pub disc_color: Vec3,
    /// The halo around the disc: radius, intensity.
    pub halo: Vec2,
    pub cloud_lit: Vec3,
    pub cloud_shadow: Vec3,
    pub cloud_opacity: f32,
    /// How far the cloud strips have scrolled, in turns.
    pub cloud_scroll: f32,
    pub backlight_weight: f32,
    /// Scene-authored linear sky colours; alpha flags whether each band is overridden.
    pub scene_zenith: Vec4,
    pub scene_horizon: Vec4,
    pub scene_nadir: Vec4,
    pub scene_rim: Vec4,
    pub scene_cloud_color: Vec4,
    pub scene_sun_visible: f32,
    /// The sky lookup (`sky_lut_image`).
    pub lut: Handle<Image>,
    /// The horizon noise (`load_horizon_noise`).
    pub noise: Handle<Image>,
    /// The cloud strips, one layer each.
    pub clouds: Handle<Image>,
}

impl Default for StylizedSky {
    fn default() -> Self {
        Self {
            tick: 0,
            time: 0.0,
            phase: 0.5,
            body_direction: Vec3::Y,
            moon_active: 0.0,
            disc_opacity: 0.0,
            disc_size: 0.0,
            disc_color: Vec3::ONE,
            halo: Vec2::ZERO,
            cloud_lit: Vec3::ONE,
            cloud_shadow: Vec3::ONE,
            cloud_opacity: 1.0,
            cloud_scroll: 0.0,
            backlight_weight: 0.0,
            scene_zenith: Vec4::ZERO,
            scene_horizon: Vec4::ZERO,
            scene_nadir: Vec4::ZERO,
            scene_rim: Vec4::ZERO,
            scene_cloud_color: Vec4::ZERO,
            scene_sun_visible: 1.0,
            lut: Handle::default(),
            noise: Handle::default(),
            clouds: Handle::default(),
        }
    }
}

mod uniform {
    // the ShaderType derive's layout check function is never called
    #![allow(dead_code)]

    use bevy::{prelude::*, render::render_resource::ShaderType};

    /// `StylizedSky` as the shader reads it, plus the preset's constants.
    #[derive(ShaderType)]
    pub struct StylizedSkyUniform {
        pub tick: u32,
        pub time: f32,
        pub phase: f32,
        pub moon_active: f32,
        pub body_direction: Vec3,
        pub disc_opacity: f32,
        pub disc_color: Vec3,
        pub disc_size: f32,
        pub halo: Vec2,
        pub cloud_opacity: f32,
        pub cloud_scroll: f32,
        pub cloud_lit: Vec3,
        pub backlight_weight: f32,
        pub cloud_shadow: Vec3,
        pub scene_sun_visible: f32,
        pub horizon_noise: Vec4,
        pub scene_zenith: Vec4,
        pub scene_horizon: Vec4,
        pub scene_nadir: Vec4,
        pub scene_rim: Vec4,
        pub scene_cloud_color: Vec4,
    }
}
use uniform::StylizedSkyUniform;

impl From<&StylizedSky> for StylizedSkyUniform {
    fn from(sky: &StylizedSky) -> Self {
        Self {
            tick: sky.tick,
            time: sky.time,
            phase: sky.phase,
            moon_active: sky.moon_active,
            body_direction: sky.body_direction,
            disc_opacity: sky.disc_opacity,
            disc_color: sky.disc_color,
            disc_size: sky.disc_size,
            halo: sky.halo,
            cloud_opacity: sky.cloud_opacity,
            cloud_scroll: sky.cloud_scroll,
            cloud_lit: sky.cloud_lit,
            backlight_weight: sky.backlight_weight,
            cloud_shadow: sky.cloud_shadow,
            scene_sun_visible: sky.scene_sun_visible,
            horizon_noise: HORIZON_NOISE,
            scene_zenith: sky.scene_zenith,
            scene_horizon: sky.scene_horizon,
            scene_nadir: sky.scene_nadir,
            scene_rim: sky.scene_rim,
            scene_cloud_color: sky.scene_cloud_color,
        }
    }
}

impl bevy_atmosphere::model::Atmospheric for StylizedSky {
    fn as_bind_group(
        &self,
        layout: &BindGroupLayout,
        render_device: &RenderDevice,
        images: &RenderAssets<GpuImage>,
        fallback_image: &FallbackImage,
    ) -> BindGroup {
        let mut uniform = encase::UniformBuffer::new(Vec::new());
        uniform.write(&StylizedSkyUniform::from(self)).unwrap();
        let buffer = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("stylized_sky_uniform"),
            usage: BufferUsages::COPY_DST | BufferUsages::UNIFORM,
            contents: uniform.as_ref(),
        });
        let lut = images.get(&self.lut).unwrap_or(&fallback_image.d2);
        let noise = images.get(&self.noise).unwrap_or(&fallback_image.d2);
        // an unset handle is the default image, which is no array
        let clouds = images
            .get(&self.clouds)
            .filter(|image| image.size.depth_or_array_layers > 1)
            .unwrap_or(&fallback_image.d2_array);
        render_device.create_bind_group(
            "stylized_sky_bind_group",
            layout,
            &BindGroupEntries::sequential((
                buffer.as_entire_binding(),
                &lut.texture_view,
                &lut.sampler,
                &noise.texture_view,
                &noise.sampler,
                &clouds.texture_view,
                &clouds.sampler,
            )),
        )
    }

    fn clone_dynamic(&self) -> Box<dyn bevy_atmosphere::model::Atmospheric> {
        Box::new(self.clone())
    }

    fn as_reflect(&self) -> &dyn Reflect {
        self
    }

    fn as_reflect_mut(&mut self) -> &mut dyn Reflect {
        self
    }
}

impl bevy_atmosphere::model::RegisterAtmosphereModel for StylizedSky {
    fn register(app: &mut App) {
        use std::any::TypeId;

        app.register_type::<Self>();
        let shader = app.world().resource::<AssetServer>().load(SHADER_PATH);

        let render_app = app.sub_app_mut(RenderApp);
        let render_device = render_app.world().resource::<RenderDevice>();
        let AtmosphereImageBindGroupLayout(image_bind_group_layout) = render_app
            .world()
            .resource::<AtmosphereImageBindGroupLayout>()
            .clone();
        let bind_group_layout = Self::bind_group_layout(render_device);
        let pipeline = render_app
            .world_mut()
            .resource_mut::<PipelineCache>()
            .queue_compute_pipeline(ComputePipelineDescriptor {
                label: Some("stylized_sky_compute_pipeline".into()),
                layout: vec![bind_group_layout.clone(), image_bind_group_layout],
                push_constant_ranges: vec![],
                shader,
                shader_defs: vec![],
                entry_point: "main".into(),
                zero_initialize_workgroup_memory: false,
            });
        app.world_mut()
            .resource_mut::<AppTypeRegistry>()
            .write()
            .get_mut(TypeId::of::<Self>())
            .expect("Type not registered")
            .insert(AtmosphereModelMetadata {
                id: TypeId::of::<Self>(),
                bind_group_layout,
                pipeline,
            });
    }

    fn bind_group_layout(render_device: &RenderDevice) -> BindGroupLayout {
        let filtered = TextureSampleType::Float { filterable: true };
        render_device.create_bind_group_layout(
            "stylized_sky_bind_group_layout",
            &BindGroupLayoutEntries::sequential(
                ShaderStages::COMPUTE,
                (
                    uniform_buffer::<StylizedSkyUniform>(false),
                    // the sky lookup
                    texture_2d(filtered),
                    sampler(SamplerBindingType::Filtering),
                    // the horizon noise
                    texture_2d(filtered),
                    sampler(SamplerBindingType::Filtering),
                    // the cloud strips
                    texture_2d_array(filtered),
                    sampler(SamplerBindingType::Filtering),
                ),
            ),
        )
    }
}

/// The sky lookup as the shader samples it: `SKY_LUT_WIDTH` by `SKY_LUT_ROWS`, blending
/// between elevations and between neighbouring phases, and holding its edges.
pub fn sky_lut_image() -> Image {
    let mut image = Image::new(
        Extent3d {
            width: SKY_LUT_WIDTH as u32,
            height: SKY_LUT_ROWS as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bake_sky_lut()
            .into_iter()
            .flatten()
            .flat_map(f32::to_le_bytes)
            .collect(),
        TextureFormat::Rgba32Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        label: Some("sky_lut".to_owned()),
        ..ImageSamplerDescriptor::linear()
    });
    image
}

/// The noise that lifts the lookup near the horizon: data, not colour, tiling both ways.
pub fn load_horizon_noise(asset_server: &AssetServer) -> Handle<Image> {
    asset_server.load_with_settings(HORIZON_NOISE_PATH, |settings: &mut ImageLoaderSettings| {
        settings.is_srgb = false;
        settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            label: Some("horizon_noise".to_owned()),
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            ..ImageSamplerDescriptor::linear()
        });
    })
}

/// What the GPU tests of the sky share: an app that draws the model into a small cube, and the
/// cube read back.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use bevy::render::{
        gpu_readback::{Readback, ReadbackComplete},
        render_resource::TextureUsages,
    };
    use bevy_atmosphere::{
        model::AddAtmosphereModel, pipeline::AtmosphereImage, prelude::*,
        system_param::AtmosphereMut,
    };
    use pipeline_test::{compile_pipelines, headless_app_without_embedded_assets};

    #[derive(Resource, Default)]
    struct ReadBack(Option<Vec<u8>>);

    fn tick(mut sky: AtmosphereMut<StylizedSky>) {
        sky.tick = sky.tick.wrapping_add(1);
    }

    /// A headless app with the sky model and a cube of `resolution` texels a side, a dither
    /// step further every update. Add the other plugins under test, then `start_sky`.
    pub(crate) fn sky_test_app(resolution: u32) -> App {
        let mut app = headless_app_without_embedded_assets();
        app.init_resource::<ReadBack>()
            .insert_resource(AtmosphereSettings {
                resolution,
                ..default()
            })
            .insert_resource(AtmosphereModel::new(StylizedSky::default()))
            .add_plugins(AtmospherePlugin)
            .add_systems(Update, tick);
        app
    }

    /// Finishes the app as the client does, gives the model its textures and returns the sky
    /// cube, readable.
    pub(crate) fn start_sky(app: &mut App) -> Handle<Image> {
        app.add_plugins(assets::EmbedAssetsPlugin);
        app.finish();
        app.cleanup();
        app.sub_app_mut(RenderApp)
            .init_resource::<AtmosphereImageBindGroupLayout>();
        app.add_atmosphere_model::<StylizedSky>();
        let cube = app.world().resource::<AtmosphereImage>().handle.clone();
        let mut images = app.world_mut().resource_mut::<Assets<Image>>();
        images.get_mut(&cube).unwrap().texture_descriptor.usage |= TextureUsages::COPY_SRC;
        let lut = images.add(sky_lut_image());
        let noise = load_horizon_noise(app.world().resource::<AssetServer>());
        let mut model = app.world_mut().resource_mut::<AtmosphereModel>();
        let sky = model.to_mut::<StylizedSky>().unwrap();
        sky.lut = lut;
        sky.noise = noise;
        cube
    }

    /// Changes the model and updates until the whole cube is drawn from it: the pipeline
    /// compiled, the noise loaded, then every dither step.
    pub(crate) fn draw_sky(app: &mut App, change: impl FnOnce(&mut StylizedSky)) {
        let mut model = app.world_mut().resource_mut::<AtmosphereModel>();
        let sky = model.to_mut::<StylizedSky>().unwrap();
        change(sky);
        let noise = sky.noise.clone();
        compile_pipelines(app);
        let start = std::time::Instant::now();
        while !app.world().resource::<AssetServer>().is_loaded(&noise) {
            app.update();
            assert!(
                start.elapsed().as_secs() < 30,
                "the horizon noise never loaded"
            );
        }
        for _ in 0..80 {
            app.update();
        }
    }

    /// Every texel of a cube of `size` texels a side: faces in order, rows top down. Panics on
    /// a channel that is not finite.
    pub(crate) fn read_cube(
        app: &mut App,
        handle: &Handle<Image>,
        size: usize,
        label: &str,
    ) -> Vec<[f32; 4]> {
        app.world_mut().resource_mut::<ReadBack>().0 = None;
        let entity = app
            .world_mut()
            .spawn(Readback::texture(handle.clone()))
            .observe(
                |event: Trigger<ReadbackComplete>, mut read: ResMut<ReadBack>| {
                    read.0 = Some(event.event().0.clone());
                },
            )
            .id();
        let start = std::time::Instant::now();
        let bytes = loop {
            app.update();
            if let Some(bytes) = app.world_mut().resource_mut::<ReadBack>().0.take() {
                break bytes;
            }
            assert!(
                start.elapsed().as_secs() < 30,
                "{label}: GPU readback timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        app.world_mut().despawn(entity);
        // readback rows are aligned to 256 bytes
        let stride = (size * 8).div_ceil(256) * 256;
        assert_eq!(bytes.len(), stride * size * 6, "{label}: six cubemap faces");
        let mut texels = Vec::with_capacity(size * size * 6);
        for face in 0..6 {
            for y in 0..size {
                for x in 0..size {
                    let offset = (face * size + y) * stride + x * 8;
                    let channels: [f32; 4] = std::array::from_fn(|channel| {
                        let i = offset + channel * 2;
                        half::f16::from_le_bytes([bytes[i], bytes[i + 1]]).to_f32()
                    });
                    assert!(
                        channels.iter().all(|channel| channel.is_finite()),
                        "{label}: nonfinite texel face={face} x={x} y={y}: {channels:?}"
                    );
                    texels.push(channels);
                }
            }
        }
        texels
    }

    pub(crate) fn texel(
        cube: &[[f32; 4]],
        size: usize,
        face: usize,
        x: usize,
        y: usize,
    ) -> [f32; 4] {
        cube[(face * size + y) * size + x]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        test_support::{draw_sky, read_cube, sky_test_app, start_sky, texel},
        *,
    };
    use crate::sky_preset::{day_fraction, time_to_phase};

    const SIZE: usize = 128;
    // cube faces, and the texel whose ray runs along the face's axis
    const UP: usize = 2;
    const NORTH: usize = 4;
    const CENTRE: usize = SIZE / 2;

    #[test]
    fn the_uniform_carries_the_preset_horizon_noise() {
        let uniform = StylizedSkyUniform::from(&StylizedSky::default());
        assert_eq!(uniform.horizon_noise, Vec4::new(0.1, 5.0, 10.0, 0.002));
    }

    #[test]
    fn the_lookup_image_is_the_baked_rows() {
        let image = sky_lut_image();
        assert_eq!((image.width(), image.height()), (256, 5));
        let data = image.data.as_ref().unwrap();
        assert_eq!(data.len(), 256 * 5 * 16);
        // the last texel of the day row, the zenith at noon
        let at = (2 * 256 + 255) * 16;
        let red = f32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        assert_eq!(red, bake_sky_lut()[2 * 256 + 255][0]);
    }

    /// Renders the model into its cube at noon and at midnight and reads all six faces back.
    #[test]
    #[ignore = "needs a GPU; run with --ignored"]
    fn sky_cube_is_finite() {
        let mut app = sky_test_app(SIZE as u32);
        let cube = start_sky(&mut app);
        for hours in [12.0, 0.0] {
            draw_sky(&mut app, |sky| {
                sky.phase = time_to_phase(day_fraction(hours));
                sky.time = 1234.5;
            });
            let texels = read_cube(&mut app, &cube, SIZE, &format!("{hours} h"));
            // and drawn: no texel of either sky is black
            let darkest = texels
                .iter()
                .map(|texel| texel[0] + texel[1] + texel[2])
                .fold(f32::MAX, f32::min);
            assert!(darkest > 0.0, "{hours} h: a texel is black");
        }
    }

    #[test]
    #[ignore = "needs a GPU; run with --ignored"]
    fn noon_sky_matches_the_lookup() {
        let lut = bake_sky_lut();
        let row = |row: usize| &lut[row * SKY_LUT_WIDTH..(row + 1) * SKY_LUT_WIDTH];
        let distance = |texel: [f32; 4], lut: [f32; 4]| {
            (0..3)
                .map(|channel| (texel[channel] - lut[channel]).abs())
                .fold(0.0, f32::max)
        };
        let luminance =
            |colour: [f32; 4]| 0.2126 * colour[0] + 0.7152 * colour[1] + 0.0722 * colour[2];
        let mut app = sky_test_app(SIZE as u32);
        let cube = start_sky(&mut app);

        draw_sky(&mut app, |sky| {
            sky.phase = time_to_phase(day_fraction(12.0))
        });
        let texels = read_cube(&mut app, &cube, SIZE, "noon");
        let day = row(2);
        // straight up is the end of the day row
        let zenith = texel(&texels, SIZE, UP, CENTRE, CENTRE);
        assert!(
            distance(zenith, day[SKY_LUT_WIDTH - 1]) < 1e-2,
            "noon zenith {zenith:?}, lookup {:?}",
            day[SKY_LUT_WIDTH - 1]
        );
        // the horizon is the start of the row, lifted by the noise: at most its strength
        // times the 0.64 it weighs at the horizon
        let horizon = texel(&texels, SIZE, NORTH, CENTRE, CENTRE);
        let reach = HORIZON_NOISE.x * 0.64 * SKY_LUT_WIDTH as f32;
        let nearest = (0..=(reach * 8.0).ceil() as usize)
            .map(|step| {
                let x = step as f32 / 8.0;
                let (from, to) = (day[x.floor() as usize], day[x.ceil() as usize]);
                let between: [f32; 4] =
                    std::array::from_fn(|c| from[c] + (to[c] - from[c]) * x.fract());
                distance(horizon, between)
            })
            .fold(f32::MAX, f32::min);
        assert!(
            nearest < 1e-2,
            "noon horizon {horizon:?} is {nearest} from the lookup's first {reach} texels"
        );
        assert!(
            luminance(horizon) >= luminance(day[0]) - 0.05,
            "noon horizon {horizon:?}, lookup {:?}",
            day[0]
        );
        // the noise is the texture, not its one-colour stand-in: the lift varies with azimuth
        let (low, high) = (0..SIZE)
            .map(|x| texel(&texels, SIZE, NORTH, x, CENTRE)[0])
            .fold((f32::MAX, f32::MIN), |(low, high), red| {
                (low.min(red), high.max(red))
            });
        assert!(
            high - low > 0.02,
            "the horizon is one colour: {low}..{high}"
        );
        // alpha is the cloud cover
        assert_eq!(zenith[3], 0.0);

        // the rows run night, sunrise, day, sunset, night: sunrise is the second
        draw_sky(&mut app, |sky| sky.phase = 0.25);
        let texels = read_cube(&mut app, &cube, SIZE, "sunrise");
        let zenith = texel(&texels, SIZE, UP, CENTRE, CENTRE);
        let sunrise = row(1)[SKY_LUT_WIDTH - 1];
        assert!(
            distance(zenith, sunrise) < 1e-2,
            "sunrise zenith {zenith:?}, lookup {sunrise:?}"
        );
    }
}
