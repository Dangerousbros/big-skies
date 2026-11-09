// From Orbital Laser's native build (github.com/Stink-O/orbital-laser, MIT License, Copyright (c) 2026 Stink-O): the
// fork's post pass on upstream Horizon's render hooks. Big Skies' changes: its names, and `frame.time.x` is the
// seconds since the game started, as the prelude says (Skies' near rain and star twinkle run on it).
//
// A mod's post-processing pass (capability `render`, docs/mods.md): the mod's WGSL fragment shader, after
// `mod_post.wgsl`, drawn over the world's own target (its colour and depth) into a picture of its own, which a
// shader pack's post-processing or the copy onto the screen then takes in place of the world's.
//
// A pass may also be drawn in two stages (docs/mods.md, "A half-size stage"): with an `fs_half` and an `fs_join`
// besides `fs_main`, `fs_half` is drawn into one or two RGBA16F pictures of half the width and height, and `fs_join`
// (which reads them at bindings 6 and 7, declared by the pass itself) over the world in place of `fs_main`. A build
// without the stages, or a pass without them, draws `fs_main` alone: the declarations are only the pass's, so the
// same source compiles and runs on either.
//
// The source is compiled when its version changes; one that does not validate is logged and skipped until the
// mod gives a new one. One the driver fails to build (wgpu's internal and out-of-memory errors: on Metal, its shader
// compiler service dropping the connection on a large pass) is logged and skipped too, and tried again a few
// seconds and then half a minute later, since the next try may work; never a device fault that ends the game.
// `HORIZON_DEBUG_MOD_POST_FAIL=N` makes the first N builds fail that way, to try this on any machine.
//
// The mod's data (`render.set-data`) is a read-only storage buffer, `mod_data`: one word, 0,
// until it gives some, uploaded again only when its version changes.
//
// A mod with capability `atlas` (docs/mods.md, "Block textures in a post pass") is also given the block sprite
// arrays the terrain samples, as they are (animated layers at their current frame, mip levels as the Mipmap Levels
// option leaves them): the first at binding 8, the terrain's sampler at 9 and the other seven arrays at 10 to 16
// (a 1-layer stand-in where the game has fewer). As with bindings 6 and 7 the pass declares them itself, so a pass
// that doesn't sample them compiles and runs everywhere; one that does is refused (logged) by a game without them,
// or for a mod without the capability.
use bytemuck::{Pod, Zeroable};
use std::time::{Duration, Instant};

/// What a mod's pass is given before its own source.
pub const PRELUDE: &str = r###"// Put before a mod's post pass (capability `render`, docs/mods.md): what the pass is given. The world's picture
// and depth, a uniform describing the frame, a full-screen triangle, and a few helpers. The renderer's depth is
// reversed (1 at the near plane, 0 at the far plane and for the sky); the helpers turn it back into positions.
//
// A pass may be drawn in two stages: with `@fragment fn fs_half` and `@fragment fn fs_join` besides `fs_main`, the
// game draws fs_half into one or two RGBA16F pictures half the width and height (rounded up; its locations 0 and 1),
// then fs_join over the world in place of fs_main. fs_join reads them as bindings 6 and 7, which the pass declares
// itself, so the same source compiles (and runs fs_main) on a game without the stages:
//     @group(0) @binding(6) var half_picture: texture_2d<f32>;
//     @group(0) @binding(7) var half_extra: texture_2d<f32>;
// fs_half may not read them. `frame.resolution` is the world's picture's in both stages.
//
// A mod with capability `atlas` is also given the game's block textures, the sprite arrays the terrain samples, which
// the pass declares itself too (the same source compiles on a game without them, while it doesn't sample them):
//     @group(0) @binding(8) var block_sprites: texture_2d_array<f32>;
//     @group(0) @binding(9) var block_sampler: sampler;
// and, only for a content mod's textures past the first 2048 layers, arrays 1 to 7 at bindings 10 to 16
// (`block_sprites_1` ...). Each texture is a whole square layer, its UVs 0 to 1; `atlas.block-sprites` says which
// layer a block's faces are and how their UVs run. The sampler is the terrain's (nearest, repeating, nearest mip
// level); the arrays have the mip levels the Mipmap Levels option leaves (textureNumLevels). The texels are as the
// world uses them (gamma-encoded, as scene_color is). Sample with textureSampleLevel or textureSampleGrad (a ray
// marcher's hits are not in uniform control flow). Both stages may sample them.

struct Frame {
    view: mat4x4<f32>,
    projection: mat4x4<f32>,
    inverse_view: mat4x4<f32>,
    inverse_projection: mat4x4<f32>,
    // xyz: where the camera is in the world
    camera_position: vec4<f32>,
    // width, height, 1 / width, 1 / height, of the world's picture in pixels
    resolution: vec4<f32>,
    // seconds since the game started, frames drawn, the time of day (0 sunrise, 0.25 noon), the partial tick (the one
    // the camera is drawn at: a clock of the mod's whole ticks plus w / 20 seconds moves in step with the camera)
    time: vec4<f32>,
    // xyz: towards the sun, in the world; w: the sun's angle in radians
    sun: vec4<f32>,
    sky_color: vec4<f32>,
    fog_color: vec4<f32>,
    // near plane, far plane, render distance in blocks, vertical field of view in radians
    view_params: vec4<f32>,
    // the sixteen floats the mod sets with render.set-params
    mod_params: array<vec4<f32>, 4>,
    // xyz: what camera_position leaves out of where the camera is (its f32 rounding, up to 1/128 of a block 65,536
    // out): for something far from the world's origin, (its position - camera_position) - camera_rest is where it is
    // from the camera to well under a millimetre, the difference taken first so it is exact
    camera_rest: vec4<f32>,
}
@group(0) @binding(0) var scene_color: texture_2d<f32>;
@group(0) @binding(1) var scene_depth: texture_depth_2d;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var nearest_sampler: sampler;
@group(0) @binding(4) var<uniform> frame: Frame;
// The words the mod gives with render.set-data (one word, 0, until it gives some; bitcast<f32> for floats). Read
// arrayLength(&mod_data) before indexing it.
@group(0) @binding(5) var<storage, read> mod_data: array<u32>;

struct StageIn {
    @builtin(position) position: vec4<f32>,
    // 0,0 at the top left of the picture, 1,1 at the bottom right
    @location(0) uv: vec2<f32>,
}
@vertex fn horizon_mod_post_vertex(@builtin(vertex_index) i: u32) -> StageIn {
    let corner = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var v: StageIn;
    v.position = vec4<f32>(corner.x * 2.0 - 1.0, corner.y * 2.0 - 1.0, 0.0, 1.0);
    v.uv = vec2<f32>(corner.x, 1.0 - corner.y);
    return v;
}

// The depth at `uv`, as the renderer stores it.
fn depth_at(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(scene_depth, nearest_sampler, uv, 0);
}
fn is_sky(depth: f32) -> bool {
    return depth <= 0.0;
}
// The distance from the eye along the view direction, in blocks.
fn linear_depth(depth: f32) -> f32 {
    let n = frame.view_params.x;
    let f = frame.view_params.y;
    return n * f / (depth * (f - n) + n);
}
// The point under `uv` at `depth`, in view space.
fn view_position(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let ndc = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    let p = frame.inverse_projection * ndc;
    return p.xyz / p.w;
}
// The point under `uv` at `depth`, relative to the camera in world axes (add camera_position.xyz for the world).
fn world_offset(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    return (frame.inverse_view * vec4<f32>(view_position(uv, depth), 1.0)).xyz;
}
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}
"###;

/// The `Frame` uniform of `mod_post.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
pub struct FrameUniform {
    pub view: [[f32; 4]; 4],
    pub projection: [[f32; 4]; 4],
    pub inverse_view: [[f32; 4]; 4],
    pub inverse_projection: [[f32; 4]; 4],
    pub camera_position: [f32; 4],
    pub resolution: [f32; 4],
    pub time: [f32; 4],
    pub sun: [f32; 4],
    pub sky_color: [f32; 4],
    pub fog_color: [f32; 4],
    pub view_params: [f32; 4],
    pub mod_params: [[f32; 4]; 4],
    /// What `camera_position` (f32) leaves out of where the camera is: its rounding, far from the world's origin.
    pub camera_rest: [f32; 4],
}

/// The atlas's bindings: the first sprite array, the sampler, then the other arrays.
pub const ATLAS_ARRAY: u32 = 8;
pub const ATLAS_SAMPLER: u32 = 9;
pub const ATLAS_MORE: std::ops::RangeInclusive<u32> = 10..=16;
/// The sprite arrays the atlas bindings hold (terrain.wgsl's `SPRITE_ARRAYS` bindings).
pub const ATLAS_ARRAYS: usize = 8;

/// The half-size stage's format (both its pictures).
const HALF_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

struct Output {
    size: [u32; 2],
    view: wgpu::TextureView,
    /// The half-size stage's pictures, made the first time a pass with one runs at this size.
    half: Option<[wgpu::TextureView; 2]>,
}

/// A pass's pipelines: `fs_main` alone, or the half-size stage (`fs_half`, writing `extra` + 1 pictures) and the
/// join over the world (`fs_join`).
enum Pipelines {
    Whole(wgpu::RenderPipeline),
    Split {
        half: wgpu::RenderPipeline,
        join: wgpu::RenderPipeline,
        extra: bool,
    },
}

/// How long after each driver failure a pass is tried again; once these are used up it waits for a new source.
const RETRY_AFTER: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(30)];

/// Why a pass's pipelines could not be made.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Failure {
    /// The source is at fault: it does not validate, or wgpu refuses its pipelines (a validation error, such as a
    /// binding the layout lacks). The same source fails the same way, so it waits for a new one.
    Invalid(String),
    /// The driver failed to make them (wgpu's internal and out-of-memory errors). On Metal the shader compiler
    /// service can drop its connection (XPC_ERROR_CONNECTION_INTERRUPTED) on a large pass, more so while it is busy
    /// with the game's own pipelines at startup; the next try may work.
    Driver(String),
}

impl Failure {
    /// The failure the error scopes caught, if any (a validation error first: it is the source's fault).
    fn caught(
        validation: Option<wgpu::Error>,
        internal: Option<wgpu::Error>,
        out_of_memory: Option<wgpu::Error>,
    ) -> Option<Self> {
        match (validation, internal.or(out_of_memory)) {
            (Some(e), _) => Some(Self::Invalid(e.to_string())),
            (None, Some(e)) => Some(Self::Driver(e.to_string())),
            (None, None) => None,
        }
    }
}

/// A source's version and what became of it (`P`: its pipelines; generic so the bookkeeping is tested without a
/// device).
struct Built<P> {
    version: u64,
    state: State<P>,
}

enum State<P> {
    Ready(P),
    /// Invalid: skipped until the mod gives another source.
    Refused,
    /// The driver failed `failures` times, the last at `at`: tried again `RETRY_AFTER[failures - 1]` after it.
    Failed {
        failures: u32,
        at: Instant,
    },
}

impl<P> Built<P> {
    /// Whether `version` is to be built now: it is new, or it failed in the driver and its next try is due.
    fn due(built: Option<&Self>, version: u64, now: Instant) -> bool {
        match built {
            Some(b) if b.version == version => match b.state {
                State::Failed { failures, at } => RETRY_AFTER
                    .get(failures as usize - 1)
                    .is_some_and(|&wait| now >= at + wait),
                State::Ready(_) | State::Refused => false,
            },
            _ => true,
        }
    }

    /// What became of `version` after a build that came to `result` at `now`, `built` what it was before.
    fn after(built: Option<Self>, version: u64, result: Result<P, Failure>, now: Instant) -> Self {
        let failures = Self::failures(built.as_ref(), version);
        let state = match result {
            Ok(p) => State::Ready(p),
            Err(Failure::Invalid(_)) => State::Refused,
            Err(Failure::Driver(_)) => State::Failed {
                failures: failures + 1,
                at: now,
            },
        };
        Self { version, state }
    }

    fn ready(&self) -> Option<&P> {
        match &self.state {
            State::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// The driver failures before this build (0 for a first try).
    fn failures(built: Option<&Self>, version: u64) -> u32 {
        match built {
            Some(Self {
                version: v,
                state: State::Failed { failures, .. },
            }) if *v == version => *failures,
            _ => 0,
        }
    }
}

/// What `validate` found in a pass: whether it has a half-size stage, and if so whether `fs_half` writes a second
/// picture (location 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stages {
    Whole,
    Split { extra: bool },
}

/// A pass's bind group layouts: the whole pass's (and the half-size stage's), and the join's (the same, and the
/// half-size stage's two pictures at 6 and 7).
struct Layouts {
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    join_layout: wgpu::BindGroupLayout,
    join_pipeline_layout: wgpu::PipelineLayout,
}

pub struct ModPost {
    format: wgpu::TextureFormat,
    /// Without the atlas, and with it (bindings 8 to 16).
    layouts: [Layouts; 2],
    /// The block sprite arrays and their sampler, for a pass with the atlas (`set_atlas`).
    atlas: Option<([wgpu::TextureView; ATLAS_ARRAYS], wgpu::Sampler)>,
    /// Whether the pass in `built` was built with the atlas.
    built_atlas: bool,
    linear: wgpu::Sampler,
    nearest: wgpu::Sampler,
    uniform: wgpu::Buffer,
    /// `mod_data`, and the version of the mod's data in it (0: none, the one word 0).
    data: wgpu::Buffer,
    data_version: u64,
    /// The source's version, and its pipelines or why it has none.
    built: Option<Built<Pipelines>>,
    /// Builds still to fail as the driver would (`HORIZON_DEBUG_MOD_POST_FAIL`).
    fail_builds: u32,
    output: Option<Output>,
    started: std::time::Instant,
    frames: u32,
}

impl ModPost {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let fragment = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let texture = |binding, sample_type| {
            fragment(
                binding,
                wgpu::BindingType::Texture {
                    sample_type,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            )
        };
        let entries = [
            texture(0, wgpu::TextureSampleType::Float { filterable: true }),
            texture(1, wgpu::TextureSampleType::Depth),
            fragment(
                2,
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            ),
            fragment(
                3,
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
            ),
            fragment(
                4,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
            fragment(
                5,
                wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
            ),
            texture(6, wgpu::TextureSampleType::Float { filterable: true }),
            texture(7, wgpu::TextureSampleType::Float { filterable: true }),
        ];
        let array = |binding| {
            fragment(
                binding,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
            )
        };
        let atlas: Vec<_> = std::iter::once(array(ATLAS_ARRAY))
            .chain([fragment(
                ATLAS_SAMPLER,
                wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            )])
            .chain(ATLAS_MORE.map(array))
            .collect();
        let layouts = |label, entries: &[wgpu::BindGroupLayoutEntry]| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(&layout)],
                ..Default::default()
            });
            (layout, pipeline_layout)
        };
        let both = |with_atlas: bool| {
            let extra = if with_atlas { &atlas[..] } else { &[] };
            let whole: Vec<_> = entries[..6].iter().chain(extra).copied().collect();
            let join: Vec<_> = entries.iter().chain(extra).copied().collect();
            let (layout, pipeline_layout) = layouts("Horizon mod post", &whole);
            let (join_layout, join_pipeline_layout) = layouts("Horizon mod post join", &join);
            Layouts {
                layout,
                pipeline_layout,
                join_layout,
                join_pipeline_layout,
            }
        };
        let sampler = |label, filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                mag_filter: filter,
                min_filter: filter,
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            })
        };
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Horizon mod post frame"),
            size: std::mem::size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            format,
            layouts: [both(false), both(true)],
            atlas: None,
            built_atlas: false,
            linear: sampler("Horizon mod post linear", wgpu::FilterMode::Linear),
            nearest: sampler("Horizon mod post nearest", wgpu::FilterMode::Nearest),
            uniform,
            data: data_buffer(device, &[0]),
            data_version: 0,
            built: None,
            fail_builds: std::env::var("HORIZON_DEBUG_MOD_POST_FAIL")
                .ok()
                .and_then(|n| n.trim().parse().ok())
                .unwrap_or(0),
            output: None,
            started: std::time::Instant::now(),
            frames: 0,
        }
    }

    /// The mod's data for `mod_data` (none: the one word 0), uploaded when its version is new: written into the
    /// buffer when it is the same size, else in a new buffer of its size (the bind group is made each frame).
    pub fn set_data(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: Option<(&[u32], u64)>,
    ) {
        let (words, version) = match data {
            Some((words, version)) if !words.is_empty() => (words, version),
            _ => (&[0u32][..], 0),
        };
        if version == self.data_version {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(words);
        if self.data.size() == bytes.len() as u64 {
            queue.write_buffer(&self.data, 0, bytes);
        } else {
            self.data = data_buffer(device, words);
        }
        self.data_version = version;
    }

    /// The block sprite arrays (each a `D2Array` view; the game's first, then the others or a 1-layer stand-in) and
    /// the sampler the terrain samples them with, for a pass with the atlas. Given again when the views change (the
    /// Mipmap Levels option).
    pub fn set_atlas(&mut self, arrays: &[wgpu::TextureView], sampler: &wgpu::Sampler) {
        let Some(first) = arrays.first() else {
            return;
        };
        let views = std::array::from_fn(|i| arrays.get(i).unwrap_or(first).clone());
        self.atlas = Some((views, sampler.clone()));
    }

    /// Compile `source` (without the prelude) if `version` is new, or if it failed in the driver and its next try
    /// is due; with the atlas's bindings when `atlas` (the mod has capability `atlas`). True when the pass can run;
    /// while it cannot, the frame is drawn without it.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        version: u64,
        source: &str,
        atlas: bool,
    ) -> bool {
        self.prepare_at(device, version, source, atlas, Instant::now())
    }

    fn prepare_at(
        &mut self,
        device: &wgpu::Device,
        version: u64,
        source: &str,
        atlas: bool,
        now: Instant,
    ) -> bool {
        if atlas && self.atlas.is_none() {
            return false;
        }
        // (the same source with the atlas or without is another pass: other layouts)
        if atlas != self.built_atlas {
            self.built = None;
            self.built_atlas = atlas;
        }
        if Built::due(self.built.as_ref(), version, now) {
            let before = Built::failures(self.built.as_ref(), version);
            let full = format!("{PRELUDE}\n{source}");
            let result = validate(&full)
                .map_err(Failure::Invalid)
                .and_then(|stages| self.pipelines(device, &full, stages, atlas));
            match &result {
                Ok(_) if before > 0 => {
                    log::info!("Horizon: a mod's post pass compiled on try {}", before + 1);
                }
                Ok(_) => {}
                Err(Failure::Invalid(e)) => log::warn!(
                    "Horizon: a mod's post pass did not compile: {e}\n\
                     The pass is skipped until the mod gives another."
                ),
                Err(Failure::Driver(e)) => {
                    let tries = before + 1;
                    match RETRY_AFTER.get(before as usize) {
                        Some(wait) => log::warn!(
                            "Horizon: a mod's post pass did not compile/link in the driver (try {tries}): {e}\n\
                             The pass is skipped, and tried again in {} s.",
                            wait.as_secs()
                        ),
                        None => log::warn!(
                            "Horizon: a mod's post pass did not compile/link in the driver (try {tries}): {e}\n\
                             The pass is skipped until the mod gives another."
                        ),
                    }
                }
            }
            self.built = Some(Built::after(self.built.take(), version, result, now));
        }
        self.built.as_ref().is_some_and(|b| b.ready().is_some())
    }

    /// The pass's pipelines, made under error scopes for every kind of error: a pass whose bindings do not match the
    /// layout, or one the driver fails to compile, is an error to log, not a device fault (with no scope wgpu's
    /// default handler panics, which ends the game).
    fn pipelines(
        &mut self,
        device: &wgpu::Device,
        source: &str,
        stages: Stages,
        atlas: bool,
    ) -> Result<Pipelines, Failure> {
        let layouts = &self.layouts[usize::from(atlas)];
        // popped in the reverse order
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Horizon mod post"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let target = |format| {
            Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })
        };
        let pipeline = |layout, entry, targets: &[Option<wgpu::ColorTargetState>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("Horizon mod post"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("horizon_mod_post_vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let pipelines = match stages {
            Stages::Whole => Pipelines::Whole(pipeline(
                &layouts.pipeline_layout,
                "fs_main",
                &[target(self.format)],
            )),
            Stages::Split { extra } => Pipelines::Split {
                half: pipeline(
                    &layouts.pipeline_layout,
                    "fs_half",
                    &[target(HALF_FORMAT), target(HALF_FORMAT).filter(|_| extra)],
                ),
                join: pipeline(
                    &layouts.join_pipeline_layout,
                    "fs_join",
                    &[target(self.format)],
                ),
                extra,
            },
        };
        let validation = pollster::block_on(validation.pop());
        let internal = pollster::block_on(internal.pop());
        let out_of_memory = pollster::block_on(out_of_memory.pop());
        if let Some(failure) = Failure::caught(validation, internal, out_of_memory) {
            return Err(failure);
        }
        if self.fail_builds > 0 {
            self.fail_builds -= 1;
            return Err(Failure::Driver(
                "HORIZON_DEBUG_MOD_POST_FAIL: a driver failure, as on Metal: \
                 Compilation failed due to an interrupted connection: XPC_ERROR_CONNECTION_INTERRUPTED"
                    .into(),
            ));
        }
        Ok(pipelines)
    }

    /// Draw the pass over the world's `color` and `depth` (`size` pixels), and return its picture. Call after
    /// `prepare` said it can run.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        size: [u32; 2],
        mut frame: FrameUniform,
        target: &wgpu::TextureView,
    ) -> &wgpu::TextureView {
        if self.output.as_ref().is_none_or(|o| o.size != size) {
            let view = picture(device, "Horizon mod post picture", size, self.format);
            self.output = Some(Output {
                size,
                view,
                half: None,
            });
        }
        let split = matches!(
            self.built.as_ref().and_then(Built::ready),
            Some(Pipelines::Split { .. })
        );
        let output = self.output.as_mut().expect("made above");
        if split && output.half.is_none() {
            // (half the width and height, rounded up: each of its pixels stands for two by two of the world's)
            let half = [size[0].div_ceil(2), size[1].div_ceil(2)];
            output.half = Some([
                picture(device, "Horizon mod post half", half, HALF_FORMAT),
                picture(device, "Horizon mod post half extra", half, HALF_FORMAT),
            ]);
        }
        let output = self.output.as_ref().expect("made above");
        let (w, h) = (size[0] as f32, size[1] as f32);
        frame.resolution = [w, h, 1.0 / w, 1.0 / h];
        frame.time[0] = self.started.elapsed().as_secs_f32();
        frame.time[1] = self.frames as f32;
        self.frames = self.frames.wrapping_add(1);
        queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&frame));
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(color),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(depth),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&self.linear),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(&self.nearest),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: self.uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: self.data.as_entire_binding(),
            },
        ];
        let layouts = &self.layouts[usize::from(self.built_atlas)];
        if self.built_atlas
            && let Some((arrays, sampler)) = &self.atlas
        {
            let bindings = std::iter::once(ATLAS_ARRAY).chain(ATLAS_MORE);
            entries.extend(
                bindings
                    .zip(arrays)
                    .map(|(binding, view)| wgpu::BindGroupEntry {
                        binding,
                        resource: wgpu::BindingResource::TextureView(view),
                    }),
            );
            entries.push(wgpu::BindGroupEntry {
                binding: ATLAS_SAMPLER,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Horizon mod post"),
            layout: &layouts.layout,
            entries: &entries,
        });
        let draw = |encoder: &mut wgpu::CommandEncoder,
                    targets: &[Option<&wgpu::TextureView>],
                    pipeline: &wgpu::RenderPipeline,
                    group: &wgpu::BindGroup| {
            let attachments: Vec<_> = targets
                .iter()
                .map(|t| {
                    t.map(|view| wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Horizon mod post"),
                color_attachments: &attachments,
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        };
        match self.built.as_ref().and_then(Built::ready) {
            Some(Pipelines::Whole(pipeline)) => {
                draw(encoder, &[Some(target)], pipeline, &group);
            }
            Some(Pipelines::Split { half, join, extra }) => {
                let [first, second] = output.half.as_ref().expect("made above");
                draw(
                    encoder,
                    &[Some(first), extra.then_some(second)],
                    half,
                    &group,
                );
                entries.extend([
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(first),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(second),
                    },
                ]);
                let join_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Horizon mod post join"),
                    layout: &layouts.join_layout,
                    entries: &entries,
                });
                draw(encoder, &[Some(target)], join, &join_group);
            }
            _ => {}
        }
        &output.view
    }
}

/// A picture a pass draws into and a later one reads.
fn picture(
    device: &wgpu::Device,
    label: &str,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                // (the tests read the pictures back)
                | if cfg!(test) {
                    wgpu::TextureUsages::COPY_SRC
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// A storage buffer holding `words` (at least one).
fn data_buffer(device: &wgpu::Device, words: &[u32]) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Horizon mod post data"),
        contents: bytemuck::cast_slice(words),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    })
}

/// Parse and validate a whole pass (prelude included) before wgpu sees it, so a broken one is an error, not a
/// device fault; and find whether it has a half-size stage.
fn validate(source: &str) -> Result<Stages, String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| e.emit_to_string(source))?;
    let entry = |name: &str| {
        module
            .entry_points
            .iter()
            .position(|e| e.name == name && e.stage == naga::ShaderStage::Fragment)
    };
    if entry("fs_main").is_none() {
        return Err("no `@fragment fn fs_main`".into());
    }
    let half = match (entry("fs_half"), entry("fs_join")) {
        (None, None) => return Ok(Stages::Whole),
        (Some(half), Some(_)) => half,
        _ => {
            return Err(
                "a half-size stage needs both `@fragment fn fs_half` and `fn fs_join`".into(),
            );
        }
    };
    // the half-size stage writes its pictures at locations 0 and (if it likes) 1, and cannot read them
    let locations = match &module.entry_points[half].function.result {
        Some(naga::FunctionResult {
            binding: Some(naga::Binding::Location { location, .. }),
            ..
        }) => vec![*location],
        Some(naga::FunctionResult { ty, binding: None }) => match &module.types[*ty].inner {
            naga::TypeInner::Struct { members, .. } => members
                .iter()
                .filter_map(|m| match m.binding {
                    Some(naga::Binding::Location { location, .. }) => Some(location),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    if !locations.contains(&0) || locations.iter().any(|&l| l > 1) {
        return Err("`fs_half` writes location 0 and at most location 1".into());
    }
    let uses = info.get_entry_point(half);
    for (handle, var) in module.global_variables.iter() {
        if !uses[handle].is_empty()
            && var
                .binding
                .as_ref()
                .is_some_and(|b| b.group == 0 && (b.binding == 6 || b.binding == 7))
        {
            return Err("`fs_half` reads the pictures it draws (bindings 6 and 7)".into());
        }
    }
    Ok(Stages::Split {
        extra: locations.contains(&1),
    })
}

// ---- the pass on the renderer's hook points (Orbital Laser's bundle code, not the fork's) ---------------------------
//
// The fork drew a mod's pass inside `Renderer::draw_frame`, before the post chain. Here it is drawn on
// `native_world_drawn`, once the world is drawn, before the hand and the HUD, over the frame's own target: the
// world's picture is copied out, the pass reads the copy and draws its last stage into the target. The frame uniform
// is the fork's, from the `Frame` the renderer was given (`capture`, a HEAD on `Renderer::render`); the pass itself,
// its parameters and its data come from the game thread (`set`).

use std::sync::{Arc, Mutex};

/// What the game thread hands the renderer: the pass's source and its version, its parameters, its data and the
/// data's version, and the time of day (0 sunrise, 0.25 noon) at the last tick.
#[derive(Clone)]
pub struct Pass {
    pub source: Arc<str>,
    pub version: u64,
    pub params: [f32; 16],
    pub data: Option<(Arc<[u32]>, u64)>,
    pub day_time: u64,
}

/// What the pass reads of the frame being drawn, as the fork's `draw_frame` read it.
#[derive(Clone, Copy)]
struct Captured {
    yaw: f32,
    pitch: f32,
    fov_degrees: f32,
    bob: glam::Mat4,
    spin: glam::Mat4,
    depth_far: f32,
    distance: f32,
    sun_angle: f32,
    sky_color: [f32; 3],
    fog_color: [f32; 4],
    partial_tick: f32,
    title: bool,
}

static PASS: Mutex<Option<Pass>> = Mutex::new(None);
static CAPTURED: Mutex<Option<Captured>> = Mutex::new(None);
static DRAWER: Mutex<Option<Drawer>> = Mutex::new(None);

/// The renderer's side: the fork's `ModPost` for the target's format, and the copy of the world's picture it reads.
struct Drawer {
    format: wgpu::TextureFormat,
    post: ModPost,
    input: Option<([u32; 2], wgpu::Texture, wgpu::TextureView)>,
}

/// The game thread: the pass to draw from the next frame on, or none.
pub fn set(pass: Option<Pass>) {
    if let Ok(mut p) = PASS.lock() {
        *p = pass;
    }
}

/// Seconds since the game started (the first time this was asked): the pass's `frame.time.x`.
fn seconds() -> f32 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f32()
}

/// HEAD of `Renderer::render`: what the pass reads of this frame.
pub fn capture(frame: &crate::gfx::Frame<'_>) {
    // the fog colour as `draw_frame` works it out for the world's globals
    let fog_color = if let Some(f) = &frame.fog {
        f.color
    } else if frame.underwater {
        [frame.water_fog[0], frame.water_fog[1], frame.water_fog[2], 1.0]
    } else {
        [frame.sky.fog_color[0], frame.sky.fog_color[1], frame.sky.fog_color[2], 1.0]
    };
    let captured = Captured {
        yaw: frame.yaw,
        pitch: frame.pitch,
        fov_degrees: frame.fov_degrees,
        bob: frame.bob,
        spin: frame.spin,
        depth_far: frame.depth_far,
        distance: frame.distance,
        sun_angle: frame.sky.sun_angle,
        sky_color: frame.sky.sky_color,
        fog_color,
        partial_tick: frame.weather_time.values[2],
        title: frame.title,
    };
    if let Ok(mut c) = CAPTURED.lock() {
        *c = Some(captured);
    }
}

/// `native_render_needs`: the world's depth, while a pass is set.
pub fn needs(needs: &mut crate::gfx::native_render::RenderNeeds) {
    if PASS.lock().is_ok_and(|p| p.is_some()) {
        needs.world_depth = true;
    }
}

/// `native_world_drawn`: the pass over the world.
pub fn draw(world: &mut crate::gfx::native_render::WorldDrawn<'_>) {
    let Some(pass) = PASS.lock().ok().and_then(|p| p.clone()) else {
        return;
    };
    let Some(frame) = CAPTURED.lock().ok().and_then(|c| *c) else {
        return;
    };
    if frame.title {
        return;
    }
    let Ok(mut drawer) = DRAWER.lock() else {
        return;
    };
    if drawer.as_ref().is_none_or(|d| d.format != world.color_format) {
        *drawer = Some(Drawer {
            format: world.color_format,
            post: ModPost::new(world.device, world.color_format),
            input: None,
        });
    }
    let drawer = drawer.as_mut().expect("made above");
    // The block sprite arrays (the fork's capability `atlas`), bindings 8 to 16, sampled sharp as the fork's terrain
    // sampler did: upstream's `WorldDrawn::block_sprites`.
    let sprites = &world.block_sprites;
    drawer.post.set_atlas(sprites.views, sprites.nearest);
    if !drawer.post.prepare(world.device, pass.version, &pass.source, true) {
        return;
    }
    let size = world.size;
    if world.depth_size != size {
        // (the pass reads the depth at the picture's pixels: a world drawn smaller than the window isn't handled yet)
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| log::warn!("[mod skies] the world draws smaller than the window: no post pass"));
        return;
    }
    if drawer.input.as_ref().is_none_or(|(s, _, _)| *s != size) {
        let texture = world.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Big Skies pass input"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: world.color.format(),
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[world.color_format],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(world.color_format),
            ..Default::default()
        });
        drawer.input = Some((size, texture, view));
    }
    let (_, input, input_view) = drawer.input.as_ref().expect("made above");
    world.encoder.copy_texture_to_texture(
        world.color.as_image_copy(),
        input.as_image_copy(),
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    // The fork's uniform (gfx.rs, the mod pass): the bob's turn in `view`, its shift in `camera_position`, the
    // portal and Nausea skew in the projection.
    let aspect = size[0] as f32 / size[1].max(1) as f32;
    let turn = glam::Mat4::from_mat3(glam::Mat3::from_mat4(frame.bob));
    let view = turn * crate::camera::view_matrix(frame.yaw, frame.pitch);
    let projection =
        glam::Mat4::perspective_rh(frame.fov_degrees.to_radians(), aspect, frame.depth_far, 0.05)
            * frame.spin;
    let eye = view.inverse().transform_point3(-frame.bob.w_axis.truncate());
    let sun_angle = frame.sun_angle.to_radians();
    let sun = glam::Vec3::new(0.0, sun_angle.cos(), -sun_angle.sin());
    let sky = frame.sky_color;
    let day_fraction =
        ((pass.day_time % 24_000) as f32 + frame.partial_tick) / 24_000.0;
    let mut uniform = FrameUniform {
        view: view.to_cols_array_2d(),
        projection: projection.to_cols_array_2d(),
        inverse_view: view.inverse().to_cols_array_2d(),
        inverse_projection: projection.inverse().to_cols_array_2d(),
        camera_position: [
            (world.camera.x + f64::from(eye.x)) as f32,
            (world.camera.y + f64::from(eye.y)) as f32,
            (world.camera.z + f64::from(eye.z)) as f32,
            0.0,
        ],
        // (what the f32 camera position leaves out: its rounding, far from the world's origin)
        camera_rest: {
            let at = [
                world.camera.x + f64::from(eye.x),
                world.camera.y + f64::from(eye.y),
                world.camera.z + f64::from(eye.z),
            ];
            let rest = |v: f64| (v - f64::from(v as f32)) as f32;
            [rest(at[0]), rest(at[1]), rest(at[2]), 0.0]
        },
        time: [seconds(), 0.0, day_fraction, frame.partial_tick],
        sun: [sun.x, sun.y, sun.z, sun_angle],
        sky_color: [sky[0], sky[1], sky[2], 1.0],
        fog_color: frame.fog_color,
        view_params: [
            0.05,
            frame.depth_far,
            frame.distance,
            frame.fov_degrees.to_radians(),
        ],
        ..Default::default()
    };
    for (to, from) in uniform.mod_params.iter_mut().zip(pass.params.chunks(4)) {
        to.copy_from_slice(from);
    }
    let data = pass.data.as_ref().map(|(d, v)| (&d[..], *v));
    drawer.post.set_data(world.device, world.queue, data);
    drawer.post.run(
        world.device,
        world.queue,
        world.encoder,
        input_view,
        world.depth,
        size,
        uniform,
        world.color_view,
    );
}
