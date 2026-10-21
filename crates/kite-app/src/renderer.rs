use std::path::PathBuf;
use std::sync::Arc;

use glyphon::{
    Cache, ColorMode, FontSystem, SwashCache, TextArea, TextAtlas, TextRenderer, Viewport,
};
use wgpu::{
    AddressMode, Backends, BindGroup, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, Buffer, BufferBindingType, BufferUsages,
    ColorTargetState, ColorWrites, CommandEncoderDescriptor, CompositeAlphaMode, Device,
    DeviceDescriptor, Extent3d, FilterMode, FragmentState, Instance, InstanceDescriptor, Limits,
    LoadOp, MemoryHints, MipmapFilterMode, MultisampleState, Operations,
    PipelineCompilationOptions, PresentMode, PrimitiveState, Queue, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor, RequestAdapterOptions, Sampler,
    SamplerBindingType, SamplerDescriptor, ShaderStages, StoreOp, Surface, SurfaceColorSpace,
    SurfaceConfiguration, TexelCopyBufferLayout, Texture, TextureDescriptor, TextureDimension,
    TextureFormat, TextureSampleType, TextureUsages, TextureViewDescriptor, TextureViewDimension,
    VertexState,
};
use winit::dpi::PhysicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::theme;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct QuadVertex {
    pub position: [f32; 2],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScreenUniform {
    size: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BackgroundUniform {
    size: [f32; 2],
    image: [f32; 2],
    tint: [f32; 4],
}

const BACKGROUND_BLURRED: &[u8] = include_bytes!("../assets/background.jpg");
const BACKGROUND_SHARP: &[u8] = include_bytes!("../assets/background-neo.jpg");

fn embedded_background(wallpaper: theme::Wallpaper) -> image::RgbaImage {
    let bytes = match wallpaper {
        theme::Wallpaper::Blurred => BACKGROUND_BLURRED,
        theme::Wallpaper::Sharp => BACKGROUND_SHARP,
    };
    image::load_from_memory(bytes)
        .expect("embedded kite background is not a valid jpeg")
        .to_rgba8()
}

fn load_background(max_dimension: u32) -> (image::RgbaImage, Option<String>) {
    let embedded = || embedded_background(theme::wallpaper());
    let Some(path) = theme::background_image() else {
        return (embedded(), None);
    };
    match image::open(&path) {
        Ok(image) => {
            let image = if image.width() > max_dimension || image.height() > max_dimension {
                image.thumbnail(max_dimension, max_dimension)
            } else {
                image
            };
            (image.to_rgba8(), None)
        }
        Err(error) => (
            embedded(),
            Some(format!("background {}: {error}", path.display())),
        ),
    }
}

pub struct Renderer {
    instance: Instance,
    window: Arc<Window>,
    surface: Surface<'static>,
    device: Device,
    queue: Queue,
    config: SurfaceConfiguration,

    bg_pipeline: RenderPipeline,
    bg_layout: BindGroupLayout,
    bg_bind_group: BindGroup,
    bg_uniform_buffer: Buffer,
    bg_image_size: [f32; 2],
    background_texture: Texture,
    background_sampler: Sampler,
    background_source: (theme::Wallpaper, Option<PathBuf>),
    background_notice: Option<String>,

    pipeline: RenderPipeline,
    uniform_buffer: Buffer,
    bind_group: BindGroup,
    vertex_buffer: Buffer,
    vertex_capacity: usize,

    _cache: Cache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
}

impl Renderer {
    pub fn background_notice(&self) -> Option<&str> {
        self.background_notice.as_deref()
    }

    pub async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Self {
        let size = window.inner_size().max(PhysicalSize::new(1, 1));
        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::DX12,
            ..InstanceDescriptor::new_with_display_handle(Box::new(
                event_loop.owned_display_handle(),
            ))
        });
        let adapter = instance
            .request_adapter(&RequestAdapterOptions::default())
            .await
            .expect("no graphics adapter available");
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor {
                label: Some("kite"),
                required_features: wgpu::Features::empty(),
                required_limits: Limits::downlevel_defaults(),
                memory_hints: MemoryHints::MemoryUsage,
                ..DeviceDescriptor::default()
            })
            .await
            .expect("could not create graphics device");

        let surface = instance
            .create_surface(window.clone())
            .expect("could not create surface");
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| *format == TextureFormat::Bgra8Unorm)
            .or_else(|| {
                caps.formats
                    .iter()
                    .copied()
                    .find(|format| *format == TextureFormat::Bgra8UnormSrgb)
            })
            .unwrap_or(caps.formats[0]);
        let alpha_mode = if caps.alpha_modes.contains(&CompositeAlphaMode::Opaque) {
            CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes[0]
        };
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width,
            height: size.height,
            present_mode: PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kite quads"),
            source: wgpu::ShaderSource::Wgsl(include_str!("quad.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kite screen layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kite quad layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("kite quads"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let (background, background_notice) =
            load_background(device.limits().max_texture_dimension_2d);
        let (bg_width, bg_height) = background.dimensions();
        let background_texture = device.create_texture(&TextureDescriptor {
            label: Some("kite background"),
            size: Extent3d {
                width: bg_width,
                height: bg_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            background_texture.as_image_copy(),
            background.as_raw(),
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bg_width * 4),
                rows_per_image: Some(bg_height),
            },
            Extent3d {
                width: bg_width,
                height: bg_height,
                depth_or_array_layers: 1,
            },
        );
        let background_view = background_texture.create_view(&TextureViewDescriptor::default());
        let background_sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("kite background sampler"),
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            address_mode_w: AddressMode::ClampToEdge,
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            mipmap_filter: MipmapFilterMode::Nearest,
            ..SamplerDescriptor::default()
        });
        let bg_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kite background shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("bg.wgsl").into()),
        });
        let bg_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("kite background layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::VERTEX_FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        sample_type: TextureSampleType::Float { filterable: true },
                        view_dimension: TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 2,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bg_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kite background pipeline layout"),
            bind_group_layouts: &[Some(&bg_layout)],
            immediate_size: 0,
        });
        let bg_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("kite background pipeline"),
            layout: Some(&bg_pipeline_layout),
            vertex: VertexState {
                module: &bg_shader,
                entry_point: Some("vs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                module: &bg_shader,
                entry_point: Some("fs_main"),
                compilation_options: PipelineCompilationOptions::default(),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let bg_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kite background uniform"),
            size: std::mem::size_of::<BackgroundUniform>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bg_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kite background bind group"),
            layout: &bg_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: bg_uniform_buffer.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::TextureView(&background_view),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: BindingResource::Sampler(&background_sampler),
                },
            ],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kite screen uniform"),
            size: std::mem::size_of::<ScreenUniform>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kite screen bind group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let vertex_capacity = 4096;
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kite quad vertices"),
            size: (vertex_capacity * std::mem::size_of::<QuadVertex>()) as u64,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::with_color_mode(&device, &queue, &cache, format, ColorMode::Web);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        Self {
            instance,
            window,
            surface,
            device,
            queue,
            config,
            bg_pipeline,
            bg_layout,
            bg_bind_group,
            bg_uniform_buffer,
            bg_image_size: [bg_width as f32, bg_height as f32],
            background_texture,
            background_sampler,
            background_source: (theme::wallpaper(), theme::background_image()),
            background_notice,
            pipeline,
            uniform_buffer,
            bind_group,
            vertex_buffer,
            vertex_capacity,
            _cache: cache,
            atlas,
            text_renderer,
            viewport,
        }
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        let size = size.max(PhysicalSize::new(1, 1));
        if size.width == self.config.width && size.height == self.config.height {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Re-upload the wallpaper when a `theme` command changed its source.
    fn sync_background(&mut self) {
        let source = (theme::wallpaper(), theme::background_image());
        if source == self.background_source {
            return;
        }
        let (image, notice) = load_background(self.device.limits().max_texture_dimension_2d);
        let (width, height) = image.dimensions();
        let texture = self.device.create_texture(&TextureDescriptor {
            label: Some("kite background"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            texture.as_image_copy(),
            image.as_raw(),
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&TextureViewDescriptor::default());
        self.bg_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kite background bind group"),
            layout: &self.bg_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: self.bg_uniform_buffer.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::TextureView(&view),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: BindingResource::Sampler(&self.background_sampler),
                },
            ],
        });
        self.background_texture = texture;
        self.bg_image_size = [width as f32, height as f32];
        self.background_notice = notice;
        self.background_source = source;
    }

    pub fn render(
        &mut self,
        font_system: &mut FontSystem,
        swash_cache: &mut SwashCache,
        background_quads: &[QuadVertex],
        foreground_quads: &[QuadVertex],
        text_areas: Vec<TextArea<'_>>,
    ) {
        self.sync_background();
        self.viewport.update(
            &self.queue,
            glyphon::Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );

        if let Err(error) = self.text_renderer.prepare(
            &self.device,
            &self.queue,
            font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas,
            swash_cache,
        ) {
            eprintln!("kite: text prepare failed: {error}");
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                frame
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                if let Ok(surface) = self.instance.create_surface(self.window.clone()) {
                    self.surface = surface;
                    self.surface.configure(&self.device, &self.config);
                }
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                panic!("surface validation error");
            }
        };

        let mut vertices = Vec::with_capacity(background_quads.len() + foreground_quads.len());
        vertices.extend_from_slice(background_quads);
        vertices.extend_from_slice(foreground_quads);
        if !vertices.is_empty() {
            if vertices.len() > self.vertex_capacity {
                self.vertex_capacity = vertices.len().next_power_of_two();
                self.vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("kite quad vertices"),
                    size: (self.vertex_capacity * std::mem::size_of::<QuadVertex>()) as u64,
                    usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            self.queue
                .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        }

        let uniform = ScreenUniform {
            size: [self.config.width as f32, self.config.height as f32],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniform));

        let background = BackgroundUniform {
            size: [self.config.width as f32, self.config.height as f32],
            image: self.bg_image_size,
            tint: [
                theme::bg().r() as f32 / 255.0,
                theme::bg().g() as f32 / 255.0,
                theme::bg().b() as f32 / 255.0,
                theme::background_dim(),
            ],
        };
        self.queue
            .write_buffer(&self.bg_uniform_buffer, 0, bytemuck::bytes_of(&background));

        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("kite frame"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: theme::bg().r() as f64 / 255.0,
                            g: theme::bg().g() as f64 / 255.0,
                            b: theme::bg().b() as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.bg_bind_group, &[]);
            pass.draw(0..3, 0..1);

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            if !background_quads.is_empty() {
                pass.draw(0..background_quads.len() as u32, 0..1);
            }
            if let Err(error) =
                self.text_renderer
                    .render(&mut self.atlas, &self.viewport, &mut pass)
            {
                eprintln!("kite: text render failed: {error}");
            }
            if !foreground_quads.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                let start = background_quads.len() as u32;
                let end = start + foreground_quads.len() as u32;
                pass.draw(start..end, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
    }
}
