//! wgpu renderer: one instanced pipeline for every UI primitive, plus cached GPU soft shadows
//! (occluder mask → separable Gaussian blur per light term → composite), re-rendered only when
//! the layout changes.

use super::assets;
use super::painter::{Cmd, Inst, Layer, ShadowJob, TexId};
use super::text::{Atlas, ATLAS};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    size: [f32; 2],
    scale: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurU {
    dir: [f32; 2],
    sigma: f32,
    _pad: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShadowU {
    t: [[f32; 4]; 3],
    use_receiver: [f32; 4],
}

struct Tex {
    bind: wgpu::BindGroup,
    size: (u32, u32),
}

/// Off-screen targets and bind groups for one cached shadow.
struct ShadowSlot {
    size: (u32, u32),
    occ: wgpu::TextureView,
    rec: wgpu::TextureView,
    tmp: wgpu::TextureView,
    out: [wgpu::TextureView; 3],
    blur_u: Vec<wgpu::Buffer>,
    blur_h: [wgpu::BindGroup; 3],
    blur_v: [wgpu::BindGroup; 3],
    comp_u: wgpu::Buffer,
    comp: wgpu::BindGroup,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// The quad pipeline specialised per primitive kind (index = kind).
    quad: Vec<wgpu::RenderPipeline>,
    blur: wgpu::RenderPipeline,
    shadow: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    tex_layout: wgpu::BindGroupLayout,
    blur_layout: wgpu::BindGroupLayout,
    shadow_layout: wgpu::BindGroupLayout,
    clamp: wgpu::Sampler,
    repeat: wgpu::Sampler,
    textures: Vec<Tex>,
    names: HashMap<&'static str, TexId>,
    pub tex_sizes: Vec<(u32, u32)>,
    glyph_tex: wgpu::Texture,
    glyph_bind: wgpu::BindGroup,
    inst_buf: wgpu::Buffer,
    inst_cap: usize,
    slots: Vec<Option<ShadowSlot>>,
    /// Layout key each shadow slot was rendered for (shared with the painter).
    pub shadow_keys: Vec<u64>,
    scale: f32,
    logical: (f32, f32),
    /// The adapter in use, and whether it is the CPU (WARP) software renderer.
    pub adapter: String,
    pub software: bool,
}

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

fn premul_blend() -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
        alpha: wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add },
    }
}

impl Renderer {
    /// Creates a renderer drawing into the Win32 window `hwnd`, `phys` pixels big at `scale`.
    ///
    /// Tries a real GPU first (DirectX 12, then Vulkan), then WARP: Windows' built-in software
    /// DirectX 12 rasteriser, so PCs without a usable GPU (VMs, remote sessions, basic display
    /// drivers) still get the full UI, drawn on the CPU. Driver panics during setup are caught.
    /// `JWALA_RENDERER=software` forces WARP, `=none` simulates no renderer at all (for testing).
    pub fn new(hwnd: isize, phys: (u32, u32), scale: f32) -> Result<Renderer, String> {
        let mode = std::env::var("JWALA_RENDERER").unwrap_or_default().to_lowercase();
        // one backend per attempt: starting Vulkan alongside DX12 costs ~0.3 s (its loader and layers)
        let dx12 = (wgpu::Backends::DX12, false);
        let vulkan = (wgpu::Backends::VULKAN, false);
        let warp = (wgpu::Backends::DX12, true);
        let attempts: &[(wgpu::Backends, bool)] = match mode.as_str() {
            "software" => &[warp],
            "none" => &[],
            _ => &[dx12, vulkan, warp],
        };
        let mut errors = Vec::new();
        for &(backends, software) in attempts {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::try_new(hwnd, phys, scale, backends, software))) {
                Ok(Ok(r)) => return Ok(r),
                Ok(Err(e)) => errors.push(e),
                Err(_) => errors.push("the graphics driver crashed during setup".to_string()),
            }
        }
        Err(if errors.is_empty() { "no renderer allowed (JWALA_RENDERER=none)".into() } else { errors.join("; ") })
    }

    fn try_new(hwnd: isize, phys: (u32, u32), scale: f32, backends: wgpu::Backends, software: bool) -> Result<Renderer, String> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let surface = unsafe {
            let handle = rwh06::Win32WindowHandle::new(std::num::NonZeroIsize::new(hwnd).ok_or("null window")?);
            instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(rwh06::RawDisplayHandle::Windows(rwh06::WindowsDisplayHandle::new())),
                    raw_window_handle: rwh06::RawWindowHandle::Win32(handle),
                })
                .map_err(|e| e.to_string())?
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            // the fallback adapter on DX12 is WARP
            force_fallback_adapter: software,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("{}: {e}", if software { "software renderer" } else { "GPU" }))?;
        let info = adapter.get_info();
        // software rendering (WARP or any CPU adapter): draw at 1x and let Windows scale the window
        // up, 2.25x fewer pixels to shade at 150%
        let software = software || info.device_type == wgpu::DeviceType::Cpu;
        let (phys, scale) = if software { (((phys.0 as f32 / scale).round() as u32, (phys.1 as f32 / scale).round() as u32), 1.0) } else { (phys, scale) };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("jwala"), ..Default::default() })).map_err(|e| e.to_string())?;
        // wgpu panics on uncaptured errors by default; inside a host that would take the DAW down
        device.on_uncaptured_error(std::sync::Arc::new(|e| eprintln!("Jwala GPU error: {e}")));

        let caps = surface.get_capabilities(&adapter);
        if caps.formats.is_empty() || caps.alpha_modes.is_empty() {
            return Err(format!("{} can't draw to this window", info.name));
        }
        let format = if caps.formats.contains(&FORMAT) { FORMAT } else { caps.formats[0] };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: phys.0.max(1),
            height: phys.1.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            color_space: Default::default(),
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("ui"), source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()) });

        let uniform_entry = |binding: u32, vis: wgpu::ShaderStages| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: vis,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let smp_entry = |binding: u32| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None };

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT)] });
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[tex_entry(0), smp_entry(1), smp_entry(2)] });
        let glyph_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[tex_entry(0)] });
        let blur_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT), tex_entry(1), smp_entry(2)] });
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT), tex_entry(1), tex_entry(2), tex_entry(3), tex_entry(4), smp_entry(5)],
        });

        let inst_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4];
        let pipeline = |label: &str, layouts: &[Option<&wgpu::BindGroupLayout>], vs: &str, fs: &str, buffers: &[Option<wgpu::VertexBufferLayout>], kind: Option<f64>| {
            let constants = kind.map(|k| vec![("KIND", k)]).unwrap_or_default();
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: layouts, ..Default::default() });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() },
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: Some(premul_blend()), write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() },
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let quad: Vec<wgpu::RenderPipeline> = (0..8)
            .map(|k| {
                pipeline(
                    "quad",
                    &[Some(&globals_layout), Some(&tex_layout), Some(&glyph_layout)],
                    "vs_quad",
                    "fs_quad",
                    &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Inst>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &inst_attrs })],
                    Some(k as f64),
                )
            })
            .collect();
        let blur = pipeline("blur", &[Some(&blur_layout)], "vs_full", "fs_blur", &[], None);
        let shadow = pipeline("shadow", &[Some(&shadow_layout)], "vs_full", "fs_shadow", &[], None);

        let globals = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &globals_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }] });

        let sampler = |mode: wgpu::AddressMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: mode,
                address_mode_v: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                // anisotropic filtering is nearly free on a GPU but very costly in software
                anisotropy_clamp: if software { 1 } else { 4 },
                ..Default::default()
            })
        };
        let clamp = sampler(wgpu::AddressMode::ClampToEdge);
        let repeat = sampler(wgpu::AddressMode::Repeat);

        let glyph_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyphs"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let glyph_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &glyph_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&glyph_tex.create_view(&Default::default())) }],
        });

        let inst_cap = 4096;
        let inst_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (inst_cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut r = Renderer {
            device,
            queue,
            surface,
            config,
            quad,
            blur,
            shadow,
            globals,
            globals_bind,
            tex_layout,
            blur_layout,
            shadow_layout,
            clamp,
            repeat,
            textures: Vec::new(),
            names: HashMap::new(),
            tex_sizes: Vec::new(),
            glyph_tex,
            glyph_bind,
            inst_buf,
            inst_cap,
            slots: Vec::new(),
            shadow_keys: Vec::new(),
            scale,
            logical: (phys.0 as f32 / scale, phys.1 as f32 / scale),
            adapter: info.name.clone(),
            software,
        };
        r.upload_image("", 1, 1, &[vec![255, 255, 255, 255]]); // TexId::WHITE
        for img in assets::images() {
            r.upload_image(img.name, img.width, img.height, &img.mips);
        }
        Ok(r)
    }

    fn upload_image(&mut self, name: &'static str, w: u32, h: u32, mips: &[Vec<u8>]) {
        let tex = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some(name),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: mips.len() as u32,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::MipMajor,
            &mips.concat(),
        );
        let view = tex.create_view(&Default::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.clamp) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.repeat) },
            ],
        });
        let id = TexId(self.textures.len());
        self.textures.push(Tex { bind, size: (w, h) });
        self.tex_sizes.push((w, h));
        if !name.is_empty() {
            self.names.insert(name, id);
        }
    }

    pub fn tex(&self, name: &str) -> TexId {
        self.names.get(name).copied().unwrap_or(TexId::WHITE)
    }

    pub fn logical_size(&self) -> (f32, f32) {
        self.logical
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The window was resized to `phys` physical pixels.
    pub fn resize(&mut self, phys: (u32, u32), scale: f32) {
        let (phys, scale) = if self.software { (((phys.0 as f32 / scale).round() as u32, (phys.1 as f32 / scale).round() as u32), 1.0) } else { (phys, scale) };
        let phys = (phys.0.max(1), phys.1.max(1));
        if (self.config.width, self.config.height) != phys || self.scale != scale {
            self.config.width = phys.0;
            self.config.height = phys.1;
            self.surface.configure(&self.device, &self.config);
            self.scale = scale;
            self.logical = (phys.0 as f32 / scale, phys.1 as f32 / scale);
            // shadow targets depend on the size
            self.slots.clear();
            self.shadow_keys.clear();
        }
    }

    fn render_target(&self, w: u32, h: u32, label: &str) -> wgpu::TextureView {
        self.device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default())
    }

    /// Shadow masks are rendered at 1/div of the window: 1/2 on a GPU, 1/8 in software (the blurs
    /// cost taps × pixels, so that is ~64x less work; the shadows are soft enough not to show it).
    fn shadow_div(&self) -> u32 {
        if self.software { 8 } else { 2 }
    }

    fn shadow_size(&self) -> (u32, u32) {
        let d = self.shadow_div();
        ((self.config.width / d).max(1), (self.config.height / d).max(1))
    }

    fn make_slot(&self) -> ShadowSlot {
        // masks and blurs at half resolution: shadows are soft anyway
        let (w, h) = self.shadow_size();
        let occ = self.render_target(w, h, "shadow occluders");
        let rec = self.render_target(w, h, "shadow receiver");
        let tmp = self.render_target(w, h, "shadow tmp");
        let out = [self.render_target(w, h, "shadow 0"), self.render_target(w, h, "shadow 1"), self.render_target(w, h, "shadow 2")];
        let blur_u: Vec<wgpu::Buffer> = (0..6)
            .map(|_| self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false }))
            .collect();
        let blur_bind = |u: &wgpu::Buffer, src: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.blur_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: u.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.clamp) },
                ],
            })
        };
        let blur_h = [blur_bind(&blur_u[0], &occ), blur_bind(&blur_u[1], &occ), blur_bind(&blur_u[2], &occ)];
        let blur_v = [blur_bind(&blur_u[3], &tmp), blur_bind(&blur_u[4], &tmp), blur_bind(&blur_u[5], &tmp)];
        let comp_u = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: std::mem::size_of::<ShadowU>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let comp = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.shadow_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: comp_u.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&out[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&out[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&out[2]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&rec) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&self.clamp) },
            ],
        });
        ShadowSlot { size: (w, h), occ, rec, tmp, out, blur_u, blur_h, blur_v, comp_u, comp }
    }

    fn ensure_instances(&mut self, n: usize) {
        if n > self.inst_cap {
            self.inst_cap = n.next_power_of_two();
            self.inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: (self.inst_cap * std::mem::size_of::<Inst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
    }

    fn draw_layer<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>, layer: &Layer, base: u32) {
        let mut bound: Option<&'static str> = None;
        let _ = &mut bound;
        let mut pipeline_quad: Option<u8> = None;
        for c in &layer.cmds {
            match c {
                Cmd::Quads { tex, kind, start, end } => {
                    if pipeline_quad != Some(*kind) {
                        pass.set_pipeline(&self.quad[(*kind as usize).min(7)]);
                        pass.set_bind_group(0, &self.globals_bind, &[]);
                        pass.set_bind_group(2, &self.glyph_bind, &[]);
                        pass.set_vertex_buffer(0, self.inst_buf.slice(..));
                        pipeline_quad = Some(*kind);
                    }
                    pass.set_bind_group(1, &self.textures[tex.0].bind, &[]);
                    pass.draw(0..6, base + start..base + end);
                }
                Cmd::Shadow(slot) => {
                    if let Some(Some(s)) = self.slots.get(*slot) {
                        pass.set_pipeline(&self.shadow);
                        pass.set_bind_group(0, &s.comp, &[]);
                        pass.draw(0..3, 0..1);
                        pipeline_quad = None;
                    }
                }
            }
        }
    }

    /// Renders one frame: off-screen shadow jobs first, then the UI.
    pub fn render(&mut self, layer: &Layer, jobs: &[ShadowJob], atlas: &mut Atlas) {
        if atlas.dirty {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &self.glyph_tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &atlas.pixels,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(ATLAS), rows_per_image: Some(ATLAS) },
                wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            );
            atlas.dirty = false;
        }

        // all instances in one buffer: main layer first, then each job's layers
        let mut all: Vec<Inst> = layer.inst.clone();
        let mut job_bases = Vec::new();
        for j in jobs {
            let ob = all.len() as u32;
            all.extend_from_slice(&j.occluders.inst);
            let rb = all.len() as u32;
            if let Some(r) = &j.receiver {
                all.extend_from_slice(&r.inst);
            }
            job_bases.push((ob, rb));
        }
        self.ensure_instances(all.len().max(1));
        if !all.is_empty() {
            self.queue.write_buffer(&self.inst_buf, 0, bytemuck::cast_slice(&all));
        }
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&Globals { size: [self.logical.0, self.logical.1], scale: self.scale, _pad: 0.0 }));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());

        for (j, (ob, rb)) in jobs.iter().zip(job_bases) {
            while self.slots.len() <= j.slot {
                self.slots.push(None);
            }
            if self.slots[j.slot].as_ref().map(|s| s.size) != Some(self.shadow_size()) {
                self.slots[j.slot] = Some(self.make_slot());
            }
            let s = self.slots[j.slot].as_ref().unwrap();
            let (w, h) = (s.size.0 as f32, s.size.1 as f32);
            // blur sigma in half-res texels; offsets in uv
            let half = self.scale / self.shadow_div() as f32;
            for (k, t) in j.terms.iter().enumerate() {
                self.queue.write_buffer(&s.blur_u[k], 0, bytemuck::bytes_of(&BlurU { dir: [1.0 / w, 0.0], sigma: t.blur * half, _pad: 0.0 }));
                self.queue.write_buffer(&s.blur_u[3 + k], 0, bytemuck::bytes_of(&BlurU { dir: [0.0, 1.0 / h], sigma: t.blur * half, _pad: 0.0 }));
            }
            let mut u = ShadowU { t: [[0.0; 4]; 3], use_receiver: [j.receiver.is_some() as i32 as f32, 0.0, 0.0, 0.0] };
            for (k, t) in j.terms.iter().enumerate() {
                u.t[k] = [t.dx / self.logical.0, t.dy / self.logical.1, t.strength, 0.0];
            }
            self.queue.write_buffer(&s.comp_u, 0, bytemuck::bytes_of(&u));

            let clear = |enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView| -> () {
                drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, resolve_target: None, depth_slice: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                    ..Default::default()
                }));
            };
            let _ = clear;
            for (target, lay, base) in [(&s.occ, Some(&j.occluders), ob), (&s.rec, j.receiver.as_ref(), rb)] {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("shadow mask"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, resolve_target: None, depth_slice: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                    ..Default::default()
                });
                if let Some(lay) = lay {
                    self.draw_layer(&mut pass, lay, base);
                }
            }
            for k in 0..3 {
                for (bind, target) in [(&s.blur_h[k], &s.tmp), (&s.blur_v[k], &s.out[k])] {
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("shadow blur"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: target, resolve_target: None, depth_slice: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store } })],
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.blur);
                    pass.set_bind_group(0, bind, &[]);
                    pass.draw(0..3, 0..1);
                }
            }
        }

        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            self.draw_layer(&mut pass, layer, 0);
        }
        self.queue.submit([enc.finish()]);
        self.queue.present(frame);
    }
}
