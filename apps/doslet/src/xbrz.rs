//! GPU presentation for desktop and web: xBRZ, and the non-linear 16:9 stretch
//! for any filter. The CPU source stays guest-sized.
use iced::widget::{image, shader};
use iced::{Rectangle, mouse, wgpu};

/// Which variant of the shader draws the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// xBRZ edge smoothing; otherwise nearest or bilinear sampling.
    pub smooth: bool,
    /// Non-linear horizontal stretch for a 16:9 display.
    pub warp: bool,
    /// Bilinear sampling when not smoothing.
    pub linear: bool,
}

impl Mode {
    fn index(self) -> usize {
        usize::from(self.smooth) | usize::from(self.warp) << 1 | usize::from(self.linear) << 2
    }
}

#[derive(Debug, Clone)]
pub struct Frame(pub image::Handle, pub Mode);

impl Frame {
    pub fn xbrz(handle: image::Handle, warp: bool) -> Self {
        Self(
            handle,
            Mode {
                smooth: true,
                warp,
                linear: false,
            },
        )
    }

    /// Stretch an already filtered frame; `linear` samples it bilinearly.
    pub fn stretch(handle: image::Handle, linear: bool) -> Self {
        Self(
            handle,
            Mode {
                smooth: false,
                warp: true,
                linear,
            },
        )
    }
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
    // Created on first use; most sessions need only one variant.
    render: [Option<wgpu::RenderPipeline>; 8],
    layout: wgpu::BindGroupLayout,
    guest: Option<GuestTexture>,
    bind_group: Option<wgpu::BindGroup>,
}

impl Pipeline {
    fn ensure(&mut self, device: &wgpu::Device, mode: Mode) {
        let slot = &mut self.render[mode.index()];
        if slot.is_some() {
            return;
        }
        *slot = Some(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("xBRZ"),
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
                            ("SMOOTH", f64::from(mode.smooth)),
                            ("WARP", f64::from(mode.warp)),
                            ("LINEAR", f64::from(mode.linear)),
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

/// Guest-sized RGBA texture shared by the GPU filters. It is reused until the
/// guest switches video modes; handle clones share the presentation bytes, so
/// there is no CPU upscaling or pixel clone.
pub(crate) struct GuestTexture {
    texture: wgpu::Texture,
    frame: image::Handle,
}

impl GuestTexture {
    /// Uploads `frame` unless it is current. Returns the view of a newly
    /// created texture, whose bind group the caller must rebuild.
    pub(crate) fn upload(
        slot: &mut Option<Self>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &image::Handle,
    ) -> Option<wgpu::TextureView> {
        let image::Handle::Rgba {
            id,
            width,
            height,
            pixels,
        } = frame
        else {
            return None;
        };
        if slot.as_ref().is_some_and(|guest| guest.frame.id() == *id) {
            return None;
        }
        let size = wgpu::Extent3d {
            width: *width,
            height: *height,
            depth_or_array_layers: 1,
        };
        let mut created = None;
        if slot
            .as_ref()
            .is_none_or(|guest| guest.texture.size() != size)
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("guest frame"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // The filters operate on encoded RGB, as their references do.
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            created = Some(texture.create_view(&Default::default()));
            *slot = Some(Self {
                texture,
                frame: frame.clone(),
            });
        }
        let guest = slot.as_mut().expect("guest texture initialized");
        queue.write_texture(
            guest.texture.as_image_copy(),
            pixels.as_ref(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(*width * 4),
                rows_per_image: Some(*height),
            },
            size,
        );
        guest.frame = frame.clone();
        created
    }
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
        Self {
            module,
            pipeline_layout,
            format,
            render: Default::default(),
            layout,
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
        pipeline.ensure(device, self.1);
        if let Some(view) = GuestTexture::upload(&mut pipeline.guest, device, queue, &self.0) {
            pipeline.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("xBRZ guest frame"),
                layout: &pipeline.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            }));
        }
    }

    fn draw(&self, pipeline: &Pipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let (Some(bind_group), Some(render)) =
            (&pipeline.bind_group, &pipeline.render[self.1.index()])
        {
            pass.set_pipeline(render);
            pass.set_bind_group(0, bind_group, &[]);
            // iced supplies the physical viewport and clipping, including HiDPI.
            pass.draw(0..3, 0..1);
        }
        true
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod tests {
    use super::*;
    use shader::{Pipeline as _, Primitive as _};

    pub(crate) fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
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

    /// Draws `frame` into a `width` × `height` target and reads it back.
    pub(crate) fn render<P: shader::Primitive>(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &mut P::Pipeline,
        frame: &P,
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
            let solid = Frame::xbrz(image::Handle::from_rgba(1, 1, color.to_vec()), false);
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
            let diagonal = Frame::xbrz(image::Handle::from_rgba(5, 5, pixels.clone()), false);
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
            let black = Frame::xbrz(
                image::Handle::from_rgba(5, 5, [0, 0, 0, 255].repeat(25)),
                false,
            );
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

    #[test]
    #[ignore = "requires a wgpu adapter; run explicitly for shader validation"]
    fn gpu_stretch_matches_the_cpu_curve() {
        let instance = wgpu::Instance::default();
        let adapter = block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) = block_on(adapter.request_device(&Default::default())).unwrap();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let mut pipeline = Pipeline::new(&device, &queue, format);
        // One column per source pixel, encoding its index in the red channel.
        let columns = 200u32;
        let mut pixels = Vec::new();
        for x in 0..columns {
            pixels.extend_from_slice(&[x as u8, 0, 0, 255]);
        }
        let width = 320u32;
        for linear in [false, true] {
            let frame =
                Frame::stretch(image::Handle::from_rgba(columns, 1, pixels.clone()), linear);
            let output = render(&device, &queue, &mut pipeline, &frame, width, 1, format);
            for x in 0..width {
                let u = crate::presentation::stretch_source((x as f32 + 0.5) / width as f32);
                let expected = if linear {
                    (u * columns as f32 - 0.5).clamp(0.0, (columns - 1) as f32)
                } else {
                    (u * columns as f32).floor().min((columns - 1) as f32)
                };
                let actual = f32::from(output[x as usize * 4]);
                assert!(
                    (actual - expected).abs() <= 1.01,
                    "linear={linear} x={x} actual={actual} expected={expected}"
                );
            }
        }
    }
}
