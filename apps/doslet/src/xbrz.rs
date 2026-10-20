//! GPU xBRZ presentation for desktop and web. The CPU source stays guest-sized.
use iced::widget::{image, shader};
use iced::{Rectangle, mouse, wgpu};

#[derive(Debug, Clone)]
pub struct Frame(pub image::Handle);

impl<Message> shader::Program<Message> for Frame {
    type State = ();
    type Primitive = Self;

    fn draw(&self, _: &(), _: mouse::Cursor, _: Rectangle) -> Self {
        self.clone()
    }
}

pub struct Pipeline {
    render: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    source: Option<Source>,
}

struct Source {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    frame: image::Handle,
}

impl shader::Pipeline for Pipeline {
    fn new(device: &wgpu::Device, _: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("xBRZ freescale"),
            source: wgpu::ShaderSource::Wgsl(include_str!("xbrz.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("xBRZ source"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("xBRZ"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let render = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("xBRZ"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("OUTPUT_SRGB", f64::from(format.is_srgb()))],
                    ..Default::default()
                },
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        Self {
            render,
            layout,
            source: None,
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
        let image::Handle::Rgba {
            id,
            width,
            height,
            pixels,
        } = &self.0
        else {
            return;
        };
        if pipeline
            .source
            .as_ref()
            .is_some_and(|source| source.frame.id() == *id)
        {
            return;
        }
        let size = wgpu::Extent3d {
            width: *width,
            height: *height,
            depth_or_array_layers: 1,
        };
        // Reuse GPU storage until the guest switches video modes. Handle clones
        // share the original presentation bytes; no CPU upscaling or pixel clone.
        if pipeline
            .source
            .as_ref()
            .is_none_or(|source| source.texture.size() != size)
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("xBRZ guest frame"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // The edge metric operates on encoded RGB, as in upstream xBRZ.
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("xBRZ guest frame"),
                layout: &pipeline.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            pipeline.source = Some(Source {
                texture,
                bind_group,
                frame: self.0.clone(),
            });
        }
        let source = pipeline
            .source
            .as_mut()
            .expect("source texture initialized");
        queue.write_texture(
            source.texture.as_image_copy(),
            pixels.as_ref(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(*width * 4),
                rows_per_image: Some(*height),
            },
            size,
        );
        source.frame = self.0.clone();
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let Some(source) = &pipeline.source {
            pass.set_pipeline(&pipeline.render);
            pass.set_bind_group(0, &source.bind_group, &[]);
            // iced supplies the physical viewport and clipping, including HiDPI.
            pass.draw(0..3, 0..1);
        }
        true
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use shader::{Pipeline as _, Primitive as _};

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        struct Wake(std::thread::Thread);
        impl std::task::Wake for Wake {
            fn wake(self: std::sync::Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = std::task::Waker::from(std::sync::Arc::new(Wake(std::thread::current())));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(value) => return value,
                std::task::Poll::Pending => std::thread::park(),
            }
        }
    }

    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &mut Pipeline,
        frame: &Frame,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Vec<u8> {
        frame.prepare(
            pipeline,
            device,
            queue,
            &Rectangle::with_size(iced::Size::new(width as f32, height as f32)),
            &shader::Viewport::with_physical_size(iced::Size::new(width, height), 1.0),
        );
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("xBRZ test target"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let stride = (width * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("xBRZ test readback"),
            size: u64::from(stride * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = target.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("xBRZ test"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            assert!(frame.draw(pipeline, &mut pass));
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(30)),
            })
            .unwrap();
        rx.recv().unwrap().unwrap();
        let data = slice.get_mapped_range();
        data.chunks_exact(stride as usize)
            .flat_map(|row| row[..width as usize * 4].iter().copied())
            .collect()
    }

    #[test]
    #[ignore = "requires a wgpu adapter; run explicitly for shader validation"]
    fn gpu_xbrz_preserves_colors_and_smooths_edges() {
        let instance = wgpu::Instance::default();
        let adapter = block_on(instance.request_adapter(&Default::default())).unwrap();
        eprintln!("xBRZ test adapter: {:?}", adapter.get_info());
        let (device, queue) = block_on(adapter.request_device(&Default::default())).unwrap();
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ] {
            let mut pipeline = Pipeline::new(&device, &queue, format);
            // A one-pixel image checks all border taps and target color encoding.
            let color = [100, 151, 203, 255];
            let solid = Frame(image::Handle::from_rgba(1, 1, color.to_vec()));
            let output = render(&device, &queue, &mut pipeline, &solid, 7, 9, format);
            assert!(
                output
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| *pixel == color)
            );
            // An asymmetric diagonal catches orientation and actual edge blending.
            let mut pixels = Vec::new();
            for y in 0..5 {
                for x in 0..5 {
                    pixels.extend_from_slice(if x > y {
                        &[255, 255, 255, 255]
                    } else {
                        &[0, 0, 0, 255]
                    });
                }
            }
            let diagonal = Frame(image::Handle::from_rgba(5, 5, pixels.clone()));
            let actual = render(&device, &queue, &mut pipeline, &diagonal, 5, 5, format);
            assert_eq!(actual, pixels, "1:1 must preserve orientation and pixels");
            let scaled = render(&device, &queue, &mut pipeline, &diagonal, 17, 19, format);
            assert!(
                scaled
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| p[0] > 0 && p[0] < 255),
                "xBRZ must blend diagonal edges"
            );
            assert!(scaled.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            // Redrawing an unchanged handle reuses the texture and gives identical output.
            let repeat = render(&device, &queue, &mut pipeline, &diagonal, 17, 19, format);
            assert_eq!(repeat, scaled);
            // New bytes at the same dimensions must replace the cached frame.
            let black = Frame(image::Handle::from_rgba(5, 5, [0, 0, 0, 255].repeat(25)));
            let output = render(&device, &queue, &mut pipeline, &black, 17, 19, format);
            assert!(
                output
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [0, 0, 0, 255])
            );
        }
    }
}
