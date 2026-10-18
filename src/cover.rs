//! 封面流 —— 對應 web 版的 `coverTransform()` 與 `layout()`。
//!
//! 數字刻意照抄 CSS 版，這樣兩邊的外觀對得起來：
//! `perspective: 1100px`、側移 0.60×封面寬、第二張之後每張再 0.165×、
//! 轉 58 度、Z 往後 140、縮 13%。

use std::sync::Arc;

use crate::gfx::Gpu;

/// CSS `perspective: 1100px`
pub const PERSPECTIVE: f32 = 1100.0;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CoverQuad {
    /// tx, tz, scale, rot(弧度)
    pub xform: [f32; 4],
    /// w, h, 圓角, 透視距離
    pub size: [f32; 4],
    pub tint: [f32; 4],
    /// cx, cy, 是否為倒影, 倒影長度比例
    pub extra: [f32; 4],
}

/// 一張封面相對於中央的位置換算成變換參數。
///
/// `d` 是「離中央第幾張」，可以是小數 —— 捲動時就是靠它做連續內插的。
pub fn transform(d: f32, cover_px: f32, extra_scale: f32) -> ([f32; 4], f32) {
    let ad = d.abs();
    let sign = if d < 0.0 { -1.0f32 } else { 1.0 };
    // near：0..1，從中央翻到側邊。far：第二張之後的堆疊。
    let near = ad.min(1.0);
    let far = (ad - 1.0).max(0.0);

    let x = sign * (near * cover_px * 0.60 + far * cover_px * 0.165);
    let rot = -sign * 58.0f32.to_radians() * near;
    let z = -(near * 140.0 + far.min(8.0) * 32.0);
    let scale = (1.0 - near * 0.13 - far.min(8.0) * 0.011) * extra_scale;
    ([x, z, scale, rot], scale)
}

pub struct CoverRenderer {
    pipeline: wgpu::RenderPipeline,
    tex_layout: wgpu::BindGroupLayout,
    globals: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    capacity: u64,
    items: Vec<(CoverQuad, Arc<wgpu::TextureView>)>,
}

const INITIAL_CAPACITY: u64 = 128;

impl CoverRenderer {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cover globals"),
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
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cover tex"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cover globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("cover sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::include_wgsl!("cover.wgsl"));
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&globals_layout), Some(&tex_layout)],
            immediate_size: 0,
        });
        let attrs: Vec<wgpu::VertexAttribute> = (0..4)
            .map(|i| wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: i * 16,
                shader_location: i as u32,
            })
            .collect();
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cover"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CoverQuad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cover instances"),
            size: INITIAL_CAPACITY * std::mem::size_of::<CoverQuad>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            tex_layout,
            globals,
            globals_bind,
            sampler,
            instances,
            capacity: INITIAL_CAPACITY,
            items: Vec::new(),
        }
    }

    pub fn begin(&mut self) {
        self.items.clear();
    }

    pub fn push(&mut self, q: CoverQuad, tex: &Arc<wgpu::TextureView>) {
        self.items.push((q, tex.clone()));
    }

    /// 由遠而近排好再畫。沒有深度緩衝，所以疊放順序只能靠畫的次序 ——
    /// 而封面流本來就是嚴格的「中間那張在最上面」，用畫家演算法剛好。
    pub fn sort_back_to_front(&mut self) {
        self.items.sort_by(|a, b| {
            a.0.xform[1].partial_cmp(&b.0.xform[1]).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    pub fn render(
        &mut self,
        gpu: &Gpu,
        pass: &mut wgpu::RenderPass<'_>,
        width: u32,
        height: u32,
    ) {
        if self.items.is_empty() {
            return;
        }
        let needed = self.items.len() as u64;
        if needed > self.capacity {
            self.capacity = needed.next_power_of_two();
            self.instances = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cover instances"),
                size: self.capacity * std::mem::size_of::<CoverQuad>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let quads: Vec<CoverQuad> = self.items.iter().map(|(q, _)| *q).collect();
        gpu.queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&quads));
        // 封面的位置與透視距離都是邏輯像素（跟 CSS 的 perspective 一樣）
        let (lw, lh) = crate::gfx::logical(width, height);
        gpu.queue.write_buffer(&self.globals, 0, bytemuck::cast_slice(&[lw, lh, 0.0, 0.0]));

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));

        // 每張封面各自一張材質，所以一張一個 draw call。
        // 二十幾次呼叫在現代 GPU 上是雜訊等級，不值得為了併批去做圖集。
        for (i, (_, tex)) in self.items.iter().enumerate() {
            let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.tex_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(tex),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            pass.set_bind_group(1, &bind, &[]);
            pass.draw(0..6, i as u32..i as u32 + 1);
        }
    }
}
