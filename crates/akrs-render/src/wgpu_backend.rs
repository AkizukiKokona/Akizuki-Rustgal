//! wgpu 立即模式 2D 渲染后端 + winit 窗口/事件系统（任务3）。
//!
//! 本模块用 [wgpu](https://crates.io/crates/wgpu) 22 + [winit](https://crates.io/crates/winit)
//! 0.29 替换 macroquad/miniquad 的 OpenGL 后端，并提供与 macroquad 立即模式绘图
//! API 同名的接口（`draw_rectangle` / `draw_texture_ex` / `clear_background` …），
//! 使 `renderer.rs` 的迁移仅限于「换 import + 去 async」，无需重写 1100 行主循环。
//!
//! ## 架构
//!
//! - **全局状态**：`Device`/`Queue`/渲染管线/绘图队列/输入状态全部存于
//!   `thread_local BACKEND: RefCell<Option<Backend>>`。macroquad 主循环单线程，
//!   thread_local 访问安全且零争用。
//! - **窗口与 Surface 生命周期**：`wgpu::Surface` 借用 `winit::Window`，故二者
//!   由 `renderer::run()` 作为局部变量持有（不进 thread_local）；每帧调用
//!   `render_frame(&surface)` 借用渲染。
//! - **事件循环**：使用 winit 0.29 的 `pump_events` 模式（`EventLoopExtPumpEvents`），
//!   允许 `loop { pump_events; 帧逻辑; present; }` 的同步结构，保留 renderer 原有
//!   `loop { … }` 形态而非重构为回调式 `ApplicationHandler`。
//! - **绘制**：单管线（三角形列表 + 纹理采样）。纯色图元使用 1×1 白纹理着色，
//!   因此矩形/线条/圆/纹理四边形共用同一份 WGSL。每帧把所有顶点写入一个动态
//!   顶点缓冲，按纹理分组 `draw` 调用。
//!
//! ## 兼容性
//!
//! `prelude` 导出的类型/函数与 macroquad `prelude` 同名同签名（按本项目实际用到
//! 的子集），`renderer.rs` 把 `use macroquad::prelude::*;` 换成
//! `use crate::wgpu_backend::prelude::*;` 即可。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use wgpu::Surface;
use winit::event::{ElementState, Event, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::Window;

// ─── 公共类型（与 macroquad 同名） ──────────────────────────────────────────

/// RGBA 颜色，分量均为 [0,1] 浮点，与 macroquad::color::Color 二进制兼容。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
    /// 转为 wgpu 清屏用的 `Color`（f64）。
    fn to_wgpu(self) -> wgpu::Color {
        wgpu::Color {
            r: self.r as f64,
            g: self.g as f64,
            b: self.b as f64,
            a: self.a as f64,
        }
    }
}

pub const WHITE: Color = Color::new(1.0, 1.0, 1.0, 1.0);
pub const BLACK: Color = Color::new(0.0, 0.0, 0.0, 1.0);
pub const RED: Color = Color::new(1.0, 0.0, 0.0, 1.0);
pub const GREEN: Color = Color::new(0.0, 1.0, 0.0, 1.0);
pub const BLUE: Color = Color::new(0.0, 0.0, 1.0, 1.0);
pub const TRANSPARENT: Color = Color::new(0.0, 0.0, 0.0, 0.0);

/// 2D 向量，与 macroquad `Vec2`（glam）字段同名。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}
impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 矩形（左上角 + 宽高），与 macroquad `Rect` 同名同字段。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 纹理过滤模式。本后端 sampler 固定 Linear，此枚举仅为兼容 API 占位。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FilterMode {
    #[default]
    Linear,
    Nearest,
}

/// `measure_text` 返回的文本尺寸，与 macroquad `TextDimensions` 同字段。
#[derive(Clone, Copy, Debug, Default)]
pub struct TextDimensions {
    pub width: f32,
    pub height: f32,
    pub offset_y: f32,
}

/// `draw_texture_ex` 的参数，与 macroquad `DrawTextureParams` 同名同字段。
#[derive(Clone, Debug, Default)]
pub struct DrawTextureParams {
    /// 目标绘制尺寸；None 表示用纹理原始尺寸。
    pub dest_size: Option<Vec2>,
    /// 源纹理像素矩形；None 表示整张。
    pub source: Option<Rect>,
    /// 旋转弧度（暂未实现，保留字段以兼容 API）。
    pub rotation: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    pub pivot: Option<Vec2>,
}

// ─── 纹理 ───────────────────────────────────────────────────────────────────

/// 纹理句柄，与 macroquad `Texture2D` 同名。`Clone` 廉价（内部 Arc）。
#[derive(Clone)]
pub struct Texture2D {
    inner: Arc<TexInner>,
}

struct TexInner {
    id: u64,
    // `view` 不被直接读取（绑定通过 bind group 完成），但必须由 `TexInner` 持有
    // 以保证底层 `wgpu::TextureView`（及对应 `wgpu::Texture`）存活，否则 bind
    // group 引用的纹理会被释放。故标记 `dead_code` 抑制未读警告。
    #[allow(dead_code)]
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

impl Texture2D {
    pub fn width(&self) -> f32 {
        self.inner.w as f32
    }
    pub fn height(&self) -> f32 {
        self.inner.h as f32
    }
    /// 兼容 macroquad：本后端 sampler 固定 Linear，此方法为空操作。
    pub fn set_filter(&self, _mode: FilterMode) {}
}

/// 顶点：位置(逻辑像素) + 颜色 + 纹理坐标。
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    color: [f32; 4],
    uv: [f32; 2],
}

/// 一条绘制命令：某纹理下连续的若干三角形顶点。
struct DrawCmd {
    tex_id: u64,
    first: u32,
    count: u32,
}

/// 透传给 `renderer::run()` 的窗口配置。
#[derive(Clone)]
pub struct WindowConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub fullscreen: bool,
    pub icon_rgba: Option<(u32, u32, Vec<u8>)>,
}

// ─── 后端状态 ────────────────────────────────────────────────────────────────

struct Backend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    /// 适配器信息（GPU 名称/设备类型/驱动/后端），供 GPU 警告检测读取。
    adapter_info: wgpu::AdapterInfo,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    /// 1×1 白纹理，纯色图元着色用。
    white_tex: Texture2D,
    /// 顶点缓冲（按需扩容）。
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: u64,
    /// 每帧的绘制命令队列。
    draw_cmds: Vec<DrawCmd>,
    vertices: Vec<Vertex>,
    /// 纹理 id → bind group 缓存。
    bind_groups: HashMap<u64, wgpu::BindGroup>,
    next_tex_id: u64,
    /// 清屏色（由 `clear_background` 设置）。
    clear_color: wgpu::Color,
    // —— 输入状态 ——
    logical_size: (f32, f32),
    scale_factor: f32,
    mouse_pos: (f32, f32),
    mouse_pressed: HashSet<MouseButton>,
    mouse_down: HashSet<MouseButton>,
    mouse_released: HashSet<MouseButton>,
    mouse_wheel_delta: (f32, f32),
    keys_pressed: HashSet<KeyCode>,
    char_queue: Vec<char>,
    // —— IME 输入法状态 ——
    // 当前预编辑（preedit）文本及其光标范围；持久状态，由 Ime::Preedit 写入，
    // 由 Ime::Commit / Ime::Disabled / 空字符串 Preedit 清空。每帧不清空。
    ime_preedit: Option<(String, Option<(usize, usize)>)>,
    // 已确认（commit）的 IME 文本队列；每帧 begin_frame 清空（与 char_queue 一致），
    // 由文本输入组件通过 `take_ime_commit` 逐串取出。
    ime_commit_queue: Vec<String>,
    quit_requested: bool,
    // —— 启动时刻（get_time 用）——
    start_time: Instant,
    // —— 窗口控制请求（由 set_fullscreen / request_new_screen_size 写入）——
    pending_fullscreen: Option<bool>,
    pending_resize: Option<(f32, f32)>,
    // —— surface 失效标志 ——
    // render_frame 取不到当前纹理（Lost/Outdated/Timeout）时置 true，
    // 由 run() 消费并强制 reconfigure_surface，避免尺寸未变但 surface
    // 丢失时永久白屏。
    surface_dirty: bool,
    // —— 帧计时 ——
    last_instant: Instant,
    frame_time: f32,
}

thread_local! {
    static BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
}

const SHADER: &str = r#"
struct Uniforms { size: vec4<f32>; };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsIn { @location(0) pos: vec2<f32>, @location(1) color: vec4<f32>, @location(2) uv: vec2<f32> };
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32>, @location(1) uv: vec2<f32> };

@vertex
fn vs(in: VsIn) -> VsOut {
    var out: VsOut;
    // 逻辑像素坐标 → 裁剪空间（左上原点，Y 向下）。
    out.pos = vec4(in.pos.x / u.size.x * 2.0 - 1.0, 1.0 - in.pos.y / u.size.y * 2.0, 0.0, 1.0);
    out.color = in.color;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv) * in.color;
}
"#;

impl Backend {
    fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        format: wgpu::TextureFormat,
        adapter_info: wgpu::AdapterInfo,
    ) -> Self {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("akrs bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("akrs pll"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("akrs shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: 32,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x2],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("akrs pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[vertex_layout],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("akrs uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("akrs sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // 1×1 白纹理，纯色图元着色用。
        let white_view = create_texture_inner(&device, &queue, 1, 1, &[255, 255, 255, 255]);

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("akrs vbuf"),
            size: 1024 * 32,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let now = Instant::now();
        // white_tex（id=0）的 bind group：构造时借用 white_view，构造结束后借用释放，
        // 随后 white_view 可 move 进 TexInner。wgpu::TextureView 不可 Clone，
        // 但 bind group 内部已 Arc 引用该 view，故 move 后仍有效。
        let white_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("akrs white bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&white_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let white_tex = Texture2D {
            inner: Arc::new(TexInner { id: 0, view: white_view, w: 1, h: 1 }),
        };
        let mut bind_groups = HashMap::new();
        bind_groups.insert(0, white_bg);
        Self {
            device,
            queue,
            format,
            adapter_info,
            pipeline,
            bind_group_layout,
            uniform_buffer,
            sampler,
            white_tex,
            vertex_buffer,
            vertex_capacity: 1024 * 32,
            draw_cmds: Vec::new(),
            vertices: Vec::new(),
            bind_groups,
            next_tex_id: 1,
            clear_color: BLACK.to_wgpu(),
            logical_size: (1.0, 1.0),
            scale_factor: 1.0,
            mouse_pos: (0.0, 0.0),
            mouse_pressed: HashSet::new(),
            mouse_down: HashSet::new(),
            mouse_released: HashSet::new(),
            mouse_wheel_delta: (0.0, 0.0),
            keys_pressed: HashSet::new(),
            char_queue: Vec::new(),
            ime_preedit: None,
            ime_commit_queue: Vec::new(),
            quit_requested: false,
            start_time: Instant::now(),
            pending_fullscreen: None,
            pending_resize: None,
            surface_dirty: false,
            last_instant: now,
            frame_time: 0.0,
        }
    }

    fn white_id(&self) -> u64 {
        self.white_tex.inner.id
    }

    /// 从 RGBA 像素创建纹理并缓存其 bind group。
    fn create_texture(&mut self, w: u32, h: u32, rgba: &[u8]) -> Texture2D {
        let id = self.next_tex_id;
        self.next_tex_id += 1;
        let view = create_texture_inner(&self.device, &self.queue, w, h, rgba);
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("akrs bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.bind_groups.insert(id, bg);
        Texture2D {
            inner: Arc::new(TexInner { id, view, w, h }),
        }
    }

    fn bind_group(&self, id: u64) -> &wgpu::BindGroup {
        self.bind_groups.get(&id).expect("texture bind group missing")
    }

    /// 把一个四边形（两个三角形）加入绘制队列。
    fn push_quad(
        &mut self,
        tex_id: u64,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
        uv: [f32; 4],
    ) {
        let [u0, v0, u1, v1] = uv;
        let c = [color.r, color.g, color.b, color.a];
        let start = self.vertices.len() as u32;
        // 两个三角形：(x,y)(x+w,y)(x+w,y+h) 与 (x,y)(x+w,y+h)(x,y+h)
        self.vertices.extend_from_slice(&[
            Vertex { pos: [x, y], color: c, uv: [u0, v0] },
            Vertex { pos: [x + w, y], color: c, uv: [u1, v0] },
            Vertex { pos: [x + w, y + h], color: c, uv: [u1, v1] },
            Vertex { pos: [x, y], color: c, uv: [u0, v0] },
            Vertex { pos: [x + w, y + h], color: c, uv: [u1, v1] },
            Vertex { pos: [x, y + h], color: c, uv: [u0, v1] },
        ]);
        self.draw_cmds.push(DrawCmd { tex_id, first: start, count: 6 });
    }

    fn begin_frame(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_instant);
        self.frame_time = dt.as_secs_f32();
        self.last_instant = now;
        // 清空本帧输入边沿与绘制队列。
        self.mouse_pressed.clear();
        self.mouse_released.clear();
        self.mouse_wheel_delta = (0.0, 0.0);
        self.keys_pressed.clear();
        self.char_queue.clear();
        // IME commit 队列每帧清空（与 char_queue 同语义：pump_events 在 begin_frame 之后填充）。
        // ime_preedit 为持久状态，不清空——由 Ime 事件自身维护。
        self.ime_commit_queue.clear();
        self.draw_cmds.clear();
        self.vertices.clear();
        self.clear_color = BLACK.to_wgpu();
        self.quit_requested = false;
    }

    fn render_frame(&mut self, surface: &Surface) {
        // 更新 uniform（屏幕逻辑尺寸）。
        let uni = [
            self.logical_size.0,
            self.logical_size.1,
            0.0,
            0.0,
        ];
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&uni));

        // 顶点缓冲按需扩容。
        let needed = (self.vertices.len() * 32) as u64;
        if needed > self.vertex_capacity {
            self.vertex_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("akrs vbuf"),
                size: needed.next_power_of_two().max(needed),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vertex_capacity = needed.next_power_of_two().max(needed);
        }
        if !self.vertices.is_empty() {
            self.queue
                .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.vertices));
        }

        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            // 显存不足：无法恢复，直接放弃本帧。
            Err(wgpu::SurfaceError::OutOfMemory) => return,
            // Lost/Outdated/Timeout：surface 与窗口尺寸不匹配或被系统回收，
            // 标记 dirty 让 run() 下一帧强制 reconfigure，否则会永久白屏。
            Err(_) => {
                self.surface_dirty = true;
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("akrs enc") });
        {
            let mut r = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("akrs pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            r.set_pipeline(&self.pipeline);
            if !self.vertices.is_empty() {
                r.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                for cmd in &self.draw_cmds {
                    r.set_bind_group(0, self.bind_group(cmd.tex_id), &[]);
                    r.draw(cmd.first..cmd.first + cmd.count, 0..1);
                }
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }
}

/// 创建一个 GPU 纹理（含 view）从 RGBA8 像素，并立即上传像素数据。
/// 返回的 view 由调用方包装进 `TexInner`（含 id）。
fn create_texture_inner(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    w: u32,
    h: u32,
    rgba: &[u8],
) -> wgpu::TextureView {
    let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("akrs tex"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    if w > 0 && h > 0 && rgba.len() >= (w as usize) * (h as usize) * 4 {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            size,
        );
    }
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

// ─── 初始化（由 renderer::run 调用） ─────────────────────────────────────────

/// 创建 wgpu instance/adapter/device，从 `window` 创建并配置 surface，
/// 把渲染状态存入 thread_local，返回（surface, 物理宽, 物理高）。
/// `surface` 借用 `window`，调用方须保证 window 存活期 ≥ surface。
pub fn init_graphics(window: &Window) -> (wgpu::Surface<'_>, wgpu::TextureFormat) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let surface = instance
        .create_surface(window)
        .unwrap_or_else(|e| {
            panic!(
                "wgpu surface 创建失败：{}\n\
                 这通常表示显卡驱动有问题或不支持硬件加速。\n\
                 请更新显卡驱动到最新版本后重试。",
                e
            )
        });

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .unwrap_or_else(|| {
        // 尝试不依赖 surface 再找一次（某些环境下 surface 兼容性筛选过严）。
        let fallback = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        }));
        fallback.unwrap_or_else(|| {
            panic!(
                "找不到可用的 wgpu 图形适配器。\n\
                 可能原因：\n\
                 1. 显卡驱动过旧或不支持 DX12/Vulkan；\n\
                 2. 系统无硬件加速 GPU；\n\
                 请更新显卡驱动后重试。"
            )
        })
    });

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("akrs device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None, // trace path
    ))
    .unwrap_or_else(|e| {
        panic!(
            "wgpu device 获取失败：{}\n\
             适配器：{:?}\n\
             可能是显卡驱动版本不满足 wgpu 最低要求，请更新驱动。",
            e,
            adapter.get_info()
        )
    });

    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|&f| f == wgpu::TextureFormat::Bgra8Unorm)
        .unwrap_or_else(|| caps.formats.first().copied().unwrap_or(wgpu::TextureFormat::Bgra8Unorm));

    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        desired_maximum_frame_latency: 2,
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    let scale = window.scale_factor() as f32;
    let logical = (
        size.width as f32 / scale,
        size.height as f32 / scale,
    );

    let adapter_info = adapter.get_info();
    let mut backend = Backend::new(device, queue, format, adapter_info);
    backend.scale_factor = scale;
    backend.logical_size = logical;

    BACKEND.with(|b| *b.borrow_mut() = Some(backend));
    (surface, format)
}

/// 重新配置 surface 尺寸（窗口大小变化时调用）。
pub fn resize(new_phys_w: u32, new_phys_h: u32) {
    BACKEND.with(|b| {
        let mut g = b.borrow_mut();
        if let Some(be) = g.as_mut() {
            if new_phys_w == 0 || new_phys_h == 0 {
                return;
            }
            be.logical_size = (new_phys_w as f32 / be.scale_factor, new_phys_h as f32 / be.scale_factor);
        }
    });
    // surface 的重新 configure 由 run() 持有 surface 时调用 reconfigure_surface。
}

/// 用当前 format 与新尺寸重新配置 surface（run() 调用）。
pub fn reconfigure_surface(surface: &wgpu::Surface, w: u32, h: u32) {
    BACKEND.with(|b| {
        let g = b.borrow();
        if let Some(be) = g.as_ref() {
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: be.format,
                width: w.max(1),
                height: h.max(1),
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                desired_maximum_frame_latency: 2,
                view_formats: vec![],
            };
            surface.configure(&be.device, &config);
        }
    });
}

/// 取出并清除待处理的窗口控制请求（全屏/尺寸），由 run() 在 pump 后应用。
pub fn take_pending_fullscreen() -> Option<bool> {
    BACKEND.with(|b| b.borrow_mut().as_mut()?.pending_fullscreen.take())
}
pub fn take_pending_resize() -> Option<(f32, f32)> {
    BACKEND.with(|b| b.borrow_mut().as_mut()?.pending_resize.take())
}
/// 取出并清除 surface 失效标志（render_frame 取纹理失败时置位）。
/// run() 据此在尺寸未变但 surface 丢失时强制 reconfigure。
pub fn take_surface_dirty() -> bool {
    BACKEND.with(|b| {
        let mut g = b.borrow_mut();
        if let Some(be) = g.as_mut() {
            let dirty = be.surface_dirty;
            be.surface_dirty = false;
            dirty
        } else {
            false
        }
    })
}

// ─── 事件处理（pump_events 回调） ─────────────────────────────────────────────

pub fn handle_event(event: Event<()>) {
    BACKEND.with(|b| {
        let mut g = b.borrow_mut();
        let Some(be) = g.as_mut() else { return };
        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    be.quit_requested = true;
                }
                WindowEvent::Resized(size) => {
                    be.logical_size = (
                        size.width as f32 / be.scale_factor,
                        size.height as f32 / be.scale_factor,
                    );
                }
                WindowEvent::ScaleFactorChanged { scale_factor, inner_size_writer: _ } => {
                    be.scale_factor = scale_factor as f32;
                }
                WindowEvent::CursorMoved { position, .. } => {
                    be.mouse_pos = (
                        position.x as f32 / be.scale_factor,
                        position.y as f32 / be.scale_factor,
                    );
                }
                WindowEvent::MouseInput { state, button, .. } => {
                    match state {
                        ElementState::Pressed => {
                            be.mouse_pressed.insert(button);
                            be.mouse_down.insert(button);
                        }
                        ElementState::Released => {
                            be.mouse_released.insert(button);
                            be.mouse_down.remove(&button);
                        }
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    match delta {
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            be.mouse_wheel_delta.0 += x;
                            be.mouse_wheel_delta.1 += y;
                        }
                        winit::event::MouseScrollDelta::PixelDelta(pos) => {
                            be.mouse_wheel_delta.0 += pos.x as f32;
                            be.mouse_wheel_delta.1 += pos.y as f32;
                        }
                    }
                }
                WindowEvent::KeyboardInput { event: KeyEvent { physical_key, state, text, .. }, .. } => {
                    if state == ElementState::Pressed {
                        if let PhysicalKey::Code(kc) = physical_key {
                            be.keys_pressed.insert(kc);
                        }
                        if let Some(s) = text {
                            for c in s.chars() {
                                if !c.is_control() {
                                    be.char_queue.push(c);
                                }
                            }
                        }
                    }
                }
                WindowEvent::Ime(ime) => {
                    // winit 0.29 的 Ime 事件四变体：Enabled / Preedit / Commit / Disabled。
                    // IME 必须先经 `Window::set_ime_allowed(true)` 启用才会发送这些事件
                    // （由 renderer 主循环根据文本输入聚焦状态切换）。
                    match ime {
                        winit::event::Ime::Enabled => {
                            // IME 会话开始：无状态需要记录（preedit 仍为空）。
                        }
                        winit::event::Ime::Preedit(s, cursor) => {
                            // 预编辑文本：空字符串表示 preedit 结束，清空状态。
                            if s.is_empty() {
                                be.ime_preedit = None;
                            } else {
                                be.ime_preedit = Some((s, cursor));
                            }
                        }
                        winit::event::Ime::Commit(s) => {
                            // 确认文本：清空 preedit，整串入 commit 队列。
                            // 不再拆字符推入 char_queue——避免与非 IME 文本输入双计。
                            // 文本输入组件通过 `take_ime_commit` 读取整串。
                            be.ime_preedit = None;
                            be.ime_commit_queue.push(s);
                        }
                        winit::event::Ime::Disabled => {
                            // IME 会话结束：清空 preedit。
                            be.ime_preedit = None;
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    });
}

/// 每帧开头：清空输入边沿与绘制队列、更新帧时间。
pub fn begin_frame() {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            be.begin_frame();
        }
    });
}

/// 每帧末尾：渲染并 present。
pub fn render_frame(surface: &wgpu::Surface) {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            be.render_frame(surface);
        }
    });
}

/// 创建一个纹理（供 assets.rs 与 text.rs 使用）。
pub fn create_texture(w: u32, h: u32, rgba: &[u8]) -> Texture2D {
    BACKEND.with(|b| {
        let mut g = b.borrow_mut();
        let be = g.as_mut().expect("backend not initialized");
        be.create_texture(w, h, rgba)
    })
}

// ─── 公共绘图 API（与 macroquad 同名） ───────────────────────────────────────

pub fn clear_background(color: Color) {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            be.clear_color = color.to_wgpu();
        }
    });
}

pub fn draw_rectangle(x: f32, y: f32, w: f32, h: f32, color: Color) {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            be.push_quad(be.white_id(), x, y, w, h, color, [0.0, 0.0, 1.0, 1.0]);
        }
    });
}

/// 描边矩形：用 4 个细矩形拼成边框，近似 macroquad `draw_rectangle_lines`。
pub fn draw_rectangle_lines(x: f32, y: f32, w: f32, h: f32, thickness: f32, color: Color) {
    let t = thickness;
    draw_rectangle(x, y, w, t, color); // 上
    draw_rectangle(x, y + h - t, w, t, color); // 下
    draw_rectangle(x, y, t, h, color); // 左
    draw_rectangle(x + w - t, y, t, h, color); // 右
}

/// 直线：用旋转的细矩形近似（macroquad `draw_line`）。宽度由 `thickness` 给出。
pub fn draw_line(x1: f32, y1: f32, x2: f32, y2: f32, thickness: f32, color: Color) {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    // 以 (x1,y1) 为起点、沿 (dx,dy) 方向铺一条 thickness×len 的矩形。
    let nx = -dy / len; // 法线
    let ny = dx / len;
    let hx = nx * thickness * 0.5;
    let hy = ny * thickness * 0.5;
    // 四个角
    let (ax, ay) = (x1 + hx, y1 + hy);
    let (bx, by) = (x2 + hx, y2 + hy);
    let (cx, cy) = (x2 - hx, y2 - hy);
    let (dxp, dyp) = (x1 - hx, y1 - hy);
    let c = [color.r, color.g, color.b, color.a];
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            let tex_id = be.white_id();
            let start = be.vertices.len() as u32;
            be.vertices.extend_from_slice(&[
                Vertex { pos: [ax, ay], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [bx, by], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx, cy], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [ax, ay], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx, cy], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [dxp, dyp], color: c, uv: [0.0, 0.0] },
            ]);
            be.draw_cmds.push(DrawCmd { tex_id, first: start, count: 6 });
        }
    });
}

/// 圆角实心矩形（任务4 新增）。`radius` 为圆角半径，会被裁剪到 min(w,h)/2。
/// 实现方式：中心十字矩形 + 4 条边矩形 + 4 个 90° 三角扇圆角，全部用 white 纹理着色。
pub fn draw_rectangle_rounded(x: f32, y: f32, w: f32, h: f32, radius: f32, color: Color) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
    if r < 1.0 {
        // 半径太小，退化为直角矩形，避免顶点爆炸。
        draw_rectangle(x, y, w, h, color);
        return;
    }
    let c = [color.r, color.g, color.b, color.a];
    let segments_per_corner = 8u32; // 每个圆角 8 段，足够平滑
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            let tex_id = be.white_id();
            let start = be.vertices.len() as u32;

            // 中心矩形（x+r .. x+w-r, y+r .. y+h-r）
            let cx0 = x + r;
            let cx1 = x + w - r;
            let cy0 = y + r;
            let cy1 = y + h - r;
            be.vertices.extend_from_slice(&[
                Vertex { pos: [cx0, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy1], color: c, uv: [0.0, 0.0] },
            ]);

            // 上边矩形（x+r..x+w-r, y..y+r）
            be.vertices.extend_from_slice(&[
                Vertex { pos: [cx0, y], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, y], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, y], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy0], color: c, uv: [0.0, 0.0] },
            ]);
            // 下边矩形（x+r..x+w-r, y+h-r..y+h）
            be.vertices.extend_from_slice(&[
                Vertex { pos: [cx0, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, y + h], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, y + h], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, y + h], color: c, uv: [0.0, 0.0] },
            ]);
            // 左边矩形（x..x+r, y+r..y+h-r）
            be.vertices.extend_from_slice(&[
                Vertex { pos: [x, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [x, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx0, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [x, cy1], color: c, uv: [0.0, 0.0] },
            ]);
            // 右边矩形（x+w-r..x+w, y+r..y+h-r）
            be.vertices.extend_from_slice(&[
                Vertex { pos: [cx1, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [x + w, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [x + w, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy0], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [x + w, cy1], color: c, uv: [0.0, 0.0] },
                Vertex { pos: [cx1, cy1], color: c, uv: [0.0, 0.0] },
            ]);

            // 四个圆角：三角扇。圆心 = 角的中心点，从一条边到另一条边扫 90°。
            // 左上角：圆心 (x+r, y+r)，角度从 180° 到 270°（即 π 到 3π/2）
            push_corner_fan(be, x + r, y + r, r, std::f32::consts::PI, std::f32::consts::PI * 1.5, segments_per_corner, c);
            // 右上角：圆心 (x+w-r, y+r)，角度从 270° 到 360°（3π/2 到 2π）
            push_corner_fan(be, x + w - r, y + r, r, std::f32::consts::PI * 1.5, std::f32::consts::TAU, segments_per_corner, c);
            // 右下角：圆心 (x+w-r, y+h-r)，角度从 0° 到 90°（0 到 π/2）
            push_corner_fan(be, x + w - r, y + h - r, r, 0.0, std::f32::consts::FRAC_PI_2, segments_per_corner, c);
            // 左下角：圆心 (x+r, y+h-r)，角度从 90° 到 180°（π/2 到 π）
            push_corner_fan(be, x + r, y + h - r, r, std::f32::consts::FRAC_PI_2, std::f32::consts::PI, segments_per_corner, c);

            let count = be.vertices.len() as u32 - start;
            be.draw_cmds.push(DrawCmd { tex_id, first: start, count });
        }
    });
}

/// 圆角描边矩形（任务4 新增）。`thickness` 为边框粗细，`radius` 为圆角半径。
pub fn draw_rectangle_lines_rounded(x: f32, y: f32, w: f32, h: f32, radius: f32, thickness: f32, color: Color) {
    if w <= 0.0 || h <= 0.0 || thickness <= 0.0 {
        return;
    }
    let r = radius.min(w * 0.5).min(h * 0.5).max(0.0);
    if r < 1.0 {
        draw_rectangle_lines(x, y, w, h, thickness, color);
        return;
    }
    let t = thickness.min(r); // 边框粗细不超过圆角半径，避免圆角处重叠
    // 外圆角矩形 - 内圆角矩形（用两个圆角矩形相减近似：画外层，再用透明色画内层）
    // 但立即模式不支持减法。改用 8 段圆弧 + 4 条直线边拼成边框。
    let c = [color.r, color.g, color.b, color.a];
    let segments_per_corner = 8u32;

    let or = r; let ir = (r - t).max(0.0);

    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            let tex_id = be.white_id();
            let start = be.vertices.len() as u32;

            // 四条直边（梯形）：上、下、左、右
            push_trapezoid(be, x + r, y, x + w - r, y, x + w - r - t, y + t, x + r + t, y + t, c);
            push_trapezoid(be, x + r, y + h, x + w - r, y + h, x + w - r - t, y + h - t, x + r + t, y + h - t, c);
            push_trapezoid(be, x, y + r, x, y + h - r, x + t, y + h - r - t, x + t, y + r + t, c);
            push_trapezoid(be, x + w, y + r, x + w, y + h - r, x + w - t, y + h - r - t, x + w - t, y + r + t, c);

            // 四个圆角环：外弧 + 内弧组成四边形条带
            push_corner_ring(be, x + r, y + r, or, ir, std::f32::consts::PI, std::f32::consts::PI * 1.5, segments_per_corner, c);
            push_corner_ring(be, x + w - r, y + r, or, ir, std::f32::consts::PI * 1.5, std::f32::consts::TAU, segments_per_corner, c);
            push_corner_ring(be, x + w - r, y + h - r, or, ir, 0.0, std::f32::consts::FRAC_PI_2, segments_per_corner, c);
            push_corner_ring(be, x + r, y + h - r, or, ir, std::f32::consts::FRAC_PI_2, std::f32::consts::PI, segments_per_corner, c);

            let count = be.vertices.len() as u32 - start;
            be.draw_cmds.push(DrawCmd { tex_id, first: start, count });
        }
    });
}

/// 内部辅助：向顶点队列推入一个 90° 圆角三角扇（实心）。
fn push_corner_fan(be: &mut Backend, cx: f32, cy: f32, r: f32, ang_start: f32, ang_end: f32, segments: u32, color: [f32; 4]) {
    let center = Vertex { pos: [cx, cy], color, uv: [0.0, 0.0] };
    let mut prev = (cx + r * ang_start.cos(), cy + r * ang_start.sin());
    for i in 1..=segments {
        let ang = ang_start + (ang_end - ang_start) * (i as f32 / segments as f32);
        let cur = (cx + r * ang.cos(), cy + r * ang.sin());
        be.vertices.extend_from_slice(&[
            center,
            Vertex { pos: [prev.0, prev.1], color, uv: [0.0, 0.0] },
            Vertex { pos: [cur.0, cur.1], color, uv: [0.0, 0.0] },
        ]);
        prev = cur;
    }
}

/// 内部辅助：向顶点队列推入一个梯形四边形（4 个顶点 → 2 三角形）。
fn push_trapezoid(be: &mut Backend, ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32, dx: f32, dy: f32, color: [f32; 4]) {
    be.vertices.extend_from_slice(&[
        Vertex { pos: [ax, ay], color, uv: [0.0, 0.0] },
        Vertex { pos: [bx, by], color, uv: [0.0, 0.0] },
        Vertex { pos: [cx, cy], color, uv: [0.0, 0.0] },
        Vertex { pos: [ax, ay], color, uv: [0.0, 0.0] },
        Vertex { pos: [cx, cy], color, uv: [0.0, 0.0] },
        Vertex { pos: [dx, dy], color, uv: [0.0, 0.0] },
    ]);
}

/// 内部辅助：向顶点队列推入一个圆角环段（外弧 + 内弧组成的四边形条带）。
fn push_corner_ring(be: &mut Backend, cx: f32, cy: f32, r_out: f32, r_in: f32, ang_start: f32, ang_end: f32, segments: u32, color: [f32; 4]) {
    if r_in >= r_out {
        return;
    }
    let mut prev_out = (cx + r_out * ang_start.cos(), cy + r_out * ang_start.sin());
    let mut prev_in = (cx + r_in * ang_start.cos(), cy + r_in * ang_start.sin());
    for i in 1..=segments {
        let ang = ang_start + (ang_end - ang_start) * (i as f32 / segments as f32);
        let cur_out = (cx + r_out * ang.cos(), cy + r_out * ang.sin());
        let cur_in = (cx + r_in * ang.cos(), cy + r_in * ang.sin());
        be.vertices.extend_from_slice(&[
            Vertex { pos: [prev_out.0, prev_out.1], color, uv: [0.0, 0.0] },
            Vertex { pos: [cur_out.0, cur_out.1], color, uv: [0.0, 0.0] },
            Vertex { pos: [cur_in.0, cur_in.1], color, uv: [0.0, 0.0] },
            Vertex { pos: [prev_out.0, prev_out.1], color, uv: [0.0, 0.0] },
            Vertex { pos: [cur_in.0, cur_in.1], color, uv: [0.0, 0.0] },
            Vertex { pos: [prev_in.0, prev_in.1], color, uv: [0.0, 0.0] },
        ]);
        prev_out = cur_out;
        prev_in = cur_in;
    }
}

/// 实心圆：用三角扇近似（macroquad `draw_circle`）。
pub fn draw_circle(cx: f32, cy: f32, r: f32, color: Color) {
    if r <= 0.0 {
        return;
    }
    let segments = 64u32;
    let c = [color.r, color.g, color.b, color.a];
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            let tex_id = be.white_id();
            let mut prev = (cx + r, cy);
            let start = be.vertices.len() as u32;
            for i in 1..=segments {
                let ang = (i as f32 / segments as f32) * std::f32::consts::TAU;
                let cur = (cx + r * ang.cos(), cy + r * ang.sin());
                be.vertices.extend_from_slice(&[
                    Vertex { pos: [cx, cy], color: c, uv: [0.0, 0.0] },
                    Vertex { pos: [prev.0, prev.1], color: c, uv: [0.0, 0.0] },
                    Vertex { pos: [cur.0, cur.1], color: c, uv: [0.0, 0.0] },
                ]);
                prev = cur;
            }
            let count = be.vertices.len() as u32 - start;
            be.draw_cmds.push(DrawCmd { tex_id, first: start, count });
        }
    });
}

pub fn draw_texture_ex(texture: Texture2D, x: f32, y: f32, tint: Color, params: DrawTextureParams) {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            let tw = texture.width();
            let th = texture.height();
            let (sx, sy, sw, sh) = match params.source {
                Some(r) => (r.x, r.y, r.w, r.h),
                None => (0.0, 0.0, tw, th),
            };
            let (dw, dh) = match params.dest_size {
                Some(v) => (v.x, v.y),
                None => (sw, sh),
            };
            let (mut u0, mut v0) = (sx / tw, sy / th);
            let (mut u1, mut v1) = ((sx + sw) / tw, (sy + sh) / th);
            if params.flip_x {
                std::mem::swap(&mut u0, &mut u1);
            }
            if params.flip_y {
                std::mem::swap(&mut v0, &mut v1);
            }
            be.push_quad(texture.inner.id, x, y, dw, dh, tint, [u0, v0, u1, v1]);
        }
    });
}

// ─── 输入/窗口查询 API（与 macroquad 同名） ─────────────────────────────────

pub fn get_frame_time() -> f32 {
    BACKEND.with(|b| b.borrow().as_ref().map_or(0.0, |be| be.frame_time))
}
pub fn screen_width() -> f32 {
    BACKEND.with(|b| b.borrow().as_ref().map_or(1.0, |be| be.logical_size.0))
}
pub fn screen_height() -> f32 {
    BACKEND.with(|b| b.borrow().as_ref().map_or(1.0, |be| be.logical_size.1))
}
pub fn dpi_scale() -> f32 {
    BACKEND.with(|b| b.borrow().as_ref().map_or(1.0, |be| be.scale_factor))
}
/// 返回当前适配器信息（GPU 名称/设备类型/驱动/后端），未初始化时返回 None。
/// 供 `print_gpu_warning` 检测软件渲染器/虚拟 GPU 等。
pub fn get_adapter_info() -> Option<wgpu::AdapterInfo> {
    BACKEND.with(|b| b.borrow().as_ref().map(|be| be.adapter_info.clone()))
}
pub fn mouse_position() -> (f32, f32) {
    BACKEND.with(|b| b.borrow().as_ref().map_or((0.0, 0.0), |be| be.mouse_pos))
}
pub fn is_mouse_button_pressed(button: MouseButton) -> bool {
    BACKEND.with(|b| b.borrow().as_ref().map_or(false, |be| be.mouse_pressed.contains(&button)))
}
pub fn is_mouse_button_down(button: MouseButton) -> bool {
    BACKEND.with(|b| b.borrow().as_ref().map_or(false, |be| be.mouse_down.contains(&button)))
}
pub fn is_mouse_button_released(button: MouseButton) -> bool {
    BACKEND.with(|b| b.borrow().as_ref().map_or(false, |be| be.mouse_released.contains(&button)))
}
pub fn mouse_wheel() -> (f32, f32) {
    BACKEND.with(|b| b.borrow().as_ref().map_or((0.0, 0.0), |be| be.mouse_wheel_delta))
}
pub fn get_time() -> f64 {
    BACKEND.with(|b| {
        b.borrow().as_ref().map_or(0.0, |be| {
            Instant::now().duration_since(be.start_time).as_secs_f64()
        })
    })
}
pub fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}
pub fn is_key_pressed(key: KeyCode) -> bool {
    BACKEND.with(|b| b.borrow().as_ref().map_or(false, |be| be.keys_pressed.contains(&key)))
}
pub fn get_char_pressed() -> Option<char> {
    BACKEND.with(|b| b.borrow_mut().as_mut().and_then(|be| be.char_queue.pop()))
}

/// 取当前 IME 预编辑（preedit）文本及其光标范围（无则 None）。
/// preedit 为持久状态：在 Ime::Preedit 之间持续可用，由 Ime::Commit/Disabled 清空。
/// 文本输入组件聚焦时据此在光标处绘制候选串（带下划线）。
pub fn ime_preedit() -> Option<(String, Option<(usize, usize)>)> {
    BACKEND.with(|b| {
        b.borrow().as_ref().and_then(|be| {
            be.ime_preedit.as_ref().map(|(s, c)| (s.clone(), *c))
        })
    })
}

/// 取出一条已确认的 IME 文本（FIFO）。文本输入组件在聚焦时循环调用直至返回 None。
/// 与 `get_char_pressed` 同语义：取出即从队列移除。
pub fn take_ime_commit() -> Option<String> {
    BACKEND.with(|b| {
        b.borrow_mut().as_mut().and_then(|be| {
            if be.ime_commit_queue.is_empty() {
                None
            } else {
                Some(be.ime_commit_queue.remove(0))
            }
        })
    })
}
pub fn is_quit_requested() -> bool {
    BACKEND.with(|b| b.borrow().as_ref().map_or(false, |be| be.quit_requested))
}
pub fn prevent_quit() {
    // 兼容 macroquad：close 由 handle_event 捕获为 quit_requested 标志而非直接退出，
    // 故此处为空操作（语义已满足「拦截退出以便自动保存」）。
}
pub fn set_fullscreen(on: bool) {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            be.pending_fullscreen = Some(on);
        }
    });
}
pub fn request_new_screen_size(w: f32, h: f32) {
    BACKEND.with(|b| {
        if let Some(be) = b.borrow_mut().as_mut() {
            be.pending_resize = Some((w, h));
        }
    });
}

/// `EventLoop::pump_events` 的薄封装，供 `renderer::run` 调用。
pub fn pump_events(
    event_loop: &mut winit::event_loop::EventLoop<()>,
    timeout: Option<Duration>,
) -> PumpStatus {
    let status = event_loop.pump_events(timeout, |event, _elwt| {
        handle_event(event);
    });
    // 若事件循环被请求退出（如回调内调用 elwt.exit()），同步置 quit_requested，
    // 让 run() 现有的「保存 + process::exit」退出路径接管，避免退出请求被忽略
    // 导致窗口无法关闭。本程序通常不主动调用 elwt.exit()，此处为安全兜底。
    if let PumpStatus::Exit(_) = status {
        BACKEND.with(|b| {
            if let Some(be) = b.borrow_mut().as_mut() {
                be.quit_requested = true;
            }
        });
    }
    status
}

// ─── prelude：与 macroquad::prelude 同名的导出 ────────────────────────────────

pub mod prelude {
    pub use super::{
        draw_circle, draw_line, draw_rectangle, draw_rectangle_lines,
        draw_rectangle_lines_rounded, draw_rectangle_rounded, draw_texture_ex,
        get_char_pressed, get_frame_time, get_time, ime_preedit, is_key_pressed,
        is_mouse_button_down, is_mouse_button_pressed, is_mouse_button_released,
        is_quit_requested, mouse_position, mouse_wheel, prevent_quit,
        request_new_screen_size, screen_height, screen_width, set_fullscreen,
        take_ime_commit, dpi_scale, vec2,
        clear_background, Color, DrawTextureParams, FilterMode, Rect, Texture2D,
        Vec2, BLACK, BLUE, GREEN, RED, TRANSPARENT, WHITE,
    };
    pub use winit::event::MouseButton;
    pub use winit::keyboard::KeyCode;
    // 文本相关（来自 text 模块）。
    pub use crate::text::{
        draw_text, draw_text_ex, load_ttf_font_from_bytes, measure_text, Font, TextDimensions,
        TextParams,
    };
    // 旧 macroquad 的 TextParams 默认 font 是 Font(0)；WHITE 已上面导出。
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_new_and_fields() {
        let c = Color::new(0.1, 0.2, 0.3, 0.4);
        assert_eq!(c.r, 0.1);
        assert_eq!(c.g, 0.2);
        assert_eq!(c.b, 0.3);
        assert_eq!(c.a, 0.4);
    }

    #[test]
    fn color_constants() {
        assert_eq!(WHITE, Color::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(BLACK, Color::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(RED, Color::new(1.0, 0.0, 0.0, 1.0));
        assert_eq!(GREEN, Color::new(0.0, 1.0, 0.0, 1.0));
        assert_eq!(BLUE, Color::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(TRANSPARENT, Color::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn vec2_new_and_default() {
        let v = Vec2::new(3.5, -2.0);
        assert_eq!(v.x, 3.5);
        assert_eq!(v.y, -2.0);
        assert_eq!(Vec2::default(), Vec2::new(0.0, 0.0));
        assert_ne!(Vec2::new(1.0, 2.0), Vec2::new(1.0, 3.0));
    }

    #[test]
    fn rect_construct_and_default() {
        let r = Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 };
        assert_eq!((r.x, r.y, r.w, r.h), (1.0, 2.0, 3.0, 4.0));
        assert_eq!(Rect::default(), Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
    }

    #[test]
    fn filter_mode_default_is_linear() {
        // sampler 固定 Linear，默认值应为 Linear。
        assert_eq!(FilterMode::default(), FilterMode::Linear);
        assert_ne!(FilterMode::Linear, FilterMode::Nearest);
    }
}
