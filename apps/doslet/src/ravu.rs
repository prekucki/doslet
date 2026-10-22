//! GPU RAVU-Zoom presentation for desktop and web.
//!
//! `ravu/ravu.wgsl` and the weight tables are generated from bjin's
//! mpv-prescalers (LGPL-3.0-or-later, see `ravu/COPYING.LESSER`) by
//! `tools/ravu_to_wgsl.py`. The CPU source stays guest-sized.
use crate::xbrz::GuestTexture;
use iced::widget::{image, shader};
use iced::{Rectangle, mouse, wgpu};

/// Weight tables: RGBA f16 texels, `width` × `height`, linearly filtered.
const LUTS: [(&[u8], u32, u32); 2] = [
    (include_bytes!("ravu/ravu_zoom_lut3.f16"), 45, 2592),
    (include_bytes!("ravu/ravu_zoom_lut3_ar.f16"), 18, 2592),
];

#[derive(Debug, Clone)]
pub struct Frame {
    pub image: image::Handle,
    /// Non-linear horizontal stretch for a 16:9 display.
    pub warp: bool,
}

impl<Message> shader::Program<Message> for Frame {
    type State = ();
    type Primitive = Self;

    fn draw(&self, _: &(), _: mouse::Cursor, _: Rectangle) -> Self {
        self.clone()
    }
}

pub struct Pipeline {
    module: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    // Created on first use: plain and stretched.
    render: [Option<wgpu::RenderPipeline>; 2],
    layout: wgpu::BindGroupLayout,
    luts: [wgpu::TextureView; 2],
    sampler: wgpu::Sampler,
    guest: Option<GuestTexture>,
    bind_group: Option<wgpu::BindGroup>,
}

impl Pipeline {
    fn ensure(&mut self, device: &wgpu::Device, warp: bool) {
        let slot = &mut self.render[usize::from(warp)];
        if slot.is_some() {
            return;
        }
        *slot = Some(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("RAVU-Zoom"),
                layout: Some(&self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &self.module,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some("fs_main"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: self.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions {
                        constants: &[
                            ("OUTPUT_SRGB", f64::from(self.format.is_srgb())),
                            ("WARP", f64::from(warp)),
                        ],
                        ..Default::default()
                    },
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview: None,
                cache: None,
            }),
        );
    }
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RAVU-Zoom"),
            source: wgpu::ShaderSource::Wgsl(include_str!("ravu/ravu.wgsl").into()),
        });
        let texture = |binding, filterable| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RAVU-Zoom"),
            entries: &[
                texture(0, false),
                texture(1, true),
                texture(2, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("RAVU-Zoom"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        // The tables are uploaded once per renderer, not per frame.
        let luts = LUTS.map(|(bytes, width, height)| {
            debug_assert_eq!(bytes.len(), (width * height * 8) as usize);
            let size = wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("RAVU-Zoom weights"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                texture.as_image_copy(),
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 8),
                    rows_per_image: Some(height),
                },
                size,
            );
            texture.create_view(&Default::default())
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("RAVU-Zoom weights"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            module,
            pipeline_layout,
            format,
            render: Default::default(),
            layout,
            luts,
            sampler,
            guest: None,
            bind_group: None,
        }
    }
}

impl shader::Primitive for Frame {
    type Pipeline = Pipeline;

    fn prepare(
        &self,
        pipeline: &mut Pipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _: &Rectangle,
        _: &shader::Viewport,
    ) {
        pipeline.ensure(device, self.warp);
        if let Some(view) = GuestTexture::upload(&mut pipeline.guest, device, queue, &self.image) {
            pipeline.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("RAVU-Zoom guest frame"),
                layout: &pipeline.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&pipeline.luts[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&pipeline.luts[1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&pipeline.sampler),
                    },
                ],
            }));
        }
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let (Some(bind_group), Some(render)) = (
            &pipeline.bind_group,
            &pipeline.render[usize::from(self.warp)],
        ) {
            pass.set_pipeline(render);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        true
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::xbrz::tests::{block_on, render};
    use shader::Pipeline as _;

    #[test]
    #[ignore = "requires a wgpu adapter; run explicitly for shader validation"]
    fn gpu_ravu_preserves_flat_colors_and_interpolates_edges() {
        let instance = wgpu::Instance::default();
        let adapter = block_on(instance.request_adapter(&Default::default())).unwrap();
        eprintln!("RAVU test adapter: {:?}", adapter.get_info());
        let (device, queue) = block_on(adapter.request_device(&Default::default())).unwrap();
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let mut pipeline = Pipeline::new(&device, &queue, format);
            // Weights sum to one, so a flat image must stay flat (f16 tables
            // allow a rounding step).
            let color = [100u8, 151, 203, 255];
            let flat = Frame {
                image: image::Handle::from_rgba(4, 4, color.repeat(16)),
                warp: false,
            };
            let output = render(&device, &queue, &mut pipeline, &flat, 13, 11, format);
            for pixel in output.as_chunks::<4>().0 {
                for channel in 0..3 {
                    let error = (i16::from(pixel[channel]) - i16::from(color[channel])).abs();
                    assert!(error <= 1, "{pixel:?} differs from {color:?}");
                }
                assert_eq!(pixel[3], 255);
            }
            // 1:1 keeps the source pixels exactly, as upstream RAVU is skipped.
            let mut pixels = Vec::new();
            for y in 0..6 {
                for x in 0..6 {
                    pixels.extend_from_slice(if x > y {
                        &[255, 255, 255, 255]
                    } else {
                        &[0, 0, 0, 255]
                    });
                }
            }
            let edge = Frame {
                image: image::Handle::from_rgba(6, 6, pixels.clone()),
                warp: false,
            };
            assert_eq!(
                render(&device, &queue, &mut pipeline, &edge, 6, 6, format),
                pixels
            );
            // Enlarging blends across the edge instead of repeating pixels.
            let scaled = render(&device, &queue, &mut pipeline, &edge, 24, 24, format);
            assert!(
                scaled
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[0] > 16 && p[0] < 240),
                "RAVU must interpolate the edge"
            );
            let stretched = Frame {
                image: edge.image.clone(),
                warp: true,
            };
            let _ = render(&device, &queue, &mut pipeline, &stretched, 32, 18, format);
        }
    }
}
