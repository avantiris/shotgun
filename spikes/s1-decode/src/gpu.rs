//! wgpu device (optionally sharing its VkDevice with FFmpeg), the three import
//! paths and the headless import benchmark. Throwaway spike code.
#![allow(dead_code)]
use crate::dec::*;
use ash::vk::{self, Handle};
use ffmpeg_next::ffi::*;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::Instant;
use wgpu::hal::api::Vulkan;

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub ash_device: ash::Device,
    pub ash_instance: ash::Instance,
    pub phys: vk::PhysicalDevice,
    pub gfx_qf: u32,
    pub dec_qf: Option<(u32, u32, u32)>, // family, queue flags, video caps
    pub raw_queue: vk::Queue,
    pub dev_exts: Vec<&'static CStr>,
    pub inst_exts: Vec<&'static CStr>,
    cmd_pool: vk::CommandPool,
    cmd_ring: Vec<vk::CommandBuffer>,
    cmd_next: std::cell::Cell<usize>,
}

const VIDEO_EXTS: &[&CStr] = &[
    c"VK_KHR_video_queue",
    c"VK_KHR_video_decode_queue",
    c"VK_KHR_video_decode_h264",
    c"VK_KHR_video_decode_h265",
    c"VK_KHR_video_decode_av1",
    c"VK_KHR_video_decode_vp9",
    c"VK_KHR_video_maintenance1",
    c"VK_KHR_video_maintenance2",
    // FFmpeg's hwcontext_vulkan uses these when present
    c"VK_KHR_external_memory_fd",
    c"VK_EXT_external_memory_dma_buf",
    c"VK_EXT_image_drm_format_modifier",
    c"VK_KHR_external_semaphore_fd",
    c"VK_EXT_external_memory_host",
    c"VK_KHR_push_descriptor",
    c"VK_KHR_sampler_ycbcr_conversion",
];

pub fn features() -> wgpu::Features {
    wgpu::Features::TEXTURE_FORMAT_NV12
        | wgpu::Features::TEXTURE_FORMAT_P010
        | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
        | wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF
}

impl Gpu {
    /// `video`: also enable Vulkan Video decode extensions + a decode queue so
    /// FFmpeg can decode on the same VkDevice.
    pub fn new(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface>,
        video: bool,
    ) -> Gpu {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: surface,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .expect("adapter");
        let info = adapter.get_info();
        let verbose = std::env::var("S1_VERBOSE").is_ok();
        if verbose {
            eprintln!("adapter: {} ({:?}) driver {} {}", info.name, info.backend, info.driver, info.driver_info);
        }
        let feats = features();
        assert!(adapter.features().contains(feats), "missing wgpu features: {:?}", feats - adapter.features());
        let limits = adapter.limits();

        unsafe {
            let hal_adapter = adapter.as_hal::<Vulkan>().expect("vulkan adapter");
            let shared = hal_adapter.shared_instance();
            let ash_instance = shared.raw_instance().clone();
            let phys = hal_adapter.raw_physical_device();
            let inst_exts: Vec<&'static CStr> = shared.extensions().to_vec();

            // find a video decode queue family
            let n = ash_instance.get_physical_device_queue_family_properties2_len(phys);
            let mut vprops = vec![vk::QueueFamilyVideoPropertiesKHR::default(); n];
            let mut qprops: Vec<vk::QueueFamilyProperties2> = vprops
                .iter_mut()
                .map(|v| vk::QueueFamilyProperties2::default().push_next(v))
                .collect();
            ash_instance.get_physical_device_queue_family_properties2(phys, &mut qprops);
            let qflags: Vec<vk::QueueFlags> =
                qprops.iter().map(|q| q.queue_family_properties.queue_flags).collect();
            drop(qprops);
            let mut dec_qf = None;
            for (i, f) in qflags.iter().enumerate() {
                if verbose {
                    eprintln!("queue family {i}: {:?} video_ops={:?}", f, vprops[i].video_codec_operations);
                }
                if f.contains(vk::QueueFlags::VIDEO_DECODE_KHR) && dec_qf.is_none() {
                    dec_qf = Some((i as u32, f.as_raw(), vprops[i].video_codec_operations.as_raw()));
                }
            }
            let dec_qf = if video { Some(dec_qf.expect("no video decode queue family")) } else { None };

            static PRIO: [f32; 1] = [1.0];
            // Without Vulkan Video a plain wgpu device is enough: path (a) needs no custom device creation.
            let (dev_exts, ash_device, gfx_qf, raw_queue, device, queue) = if !video {
                drop(hal_adapter);
                let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: None,
                    required_features: feats,
                    required_limits: limits,
                    ..Default::default()
                }))
                .expect("request_device");
                let hd = device.as_hal::<Vulkan>().unwrap();
                let r = (hd.enabled_device_extensions().to_vec(), hd.raw_device().clone(), hd.queue_family_index(), hd.raw_queue());
                drop(hd);
                (r.0, r.1, r.2, r.3, device, queue)
            } else {
            let hal_adapter_ref = &*hal_adapter;
            let open = hal_adapter
                .open_with_callback(
                    feats,
                    &limits,
                    &wgpu::MemoryHints::default(),
                    Some(Box::new(|args| {
                        if !video {
                            return;
                        }
                        for e in VIDEO_EXTS {
                            if hal_adapter_ref.physical_device_capabilities().supports_extension(e) && !args.extensions.contains(e) {
                                args.extensions.push(e);
                            } else if !hal_adapter_ref.physical_device_capabilities().supports_extension(e) {
                                eprintln!("note: device extension {:?} not supported", e);
                            }
                        }
                        args.queue_create_infos.push(
                            vk::DeviceQueueCreateInfo::default()
                                .queue_family_index(dec_qf.unwrap().0)
                                .queue_priorities(&PRIO),
                        );
                        // FFmpeg's Vulkan code uses synchronization2 barriers/submits.
                        let sync2: &'static mut _ = Box::leak(Box::new(
                            vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true),
                        ));
                        *args.create_info = (*args.create_info).push_next(sync2);
                    })),
                )
                .expect("open device");
            let dev_exts: Vec<&'static CStr> = open.device.enabled_device_extensions().to_vec();
            let ash_device = open.device.raw_device().clone();
            let gfx_qf = open.device.queue_family_index();
            let raw_queue = open.device.raw_queue();
            drop(hal_adapter);
            let (device, queue) = adapter
                .create_device_from_hal(
                    open,
                    &wgpu::DeviceDescriptor {
                        label: None,
                        required_features: feats,
                        required_limits: limits,
                        ..Default::default()
                    },
                )
                .expect("device from hal");
            (dev_exts, ash_device, gfx_qf, raw_queue, device, queue)
            };

            let cmd_pool = ash_device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(gfx_qf)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .unwrap();
            let cmd_ring = ash_device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(cmd_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(64),
                )
                .unwrap();
            Gpu {
                instance,
                adapter,
                device,
                queue,
                ash_device,
                ash_instance,
                phys,
                gfx_qf,
                dec_qf,
                raw_queue,
                dev_exts,
                inst_exts,
                cmd_pool,
                cmd_ring,
                cmd_next: std::cell::Cell::new(0),
            }
        }
    }

    pub fn headless(video: bool) -> Gpu {
        Gpu::new(new_instance(), None, video)
    }

    /// Build an FFmpeg Vulkan hw device context on top of wgpu's VkDevice.
    pub fn ffmpeg_vulkan_device(&self) -> HwDevice {
        unsafe {
            let r = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN);
            let dc = (*r).data as *mut AVHWDeviceContext;
            let vkc = &mut *((*dc).hwctx as *mut AVVulkanDeviceContext);
            let entry = ash::Entry::load().unwrap();
            vkc.get_proc_addr = std::mem::transmute(entry.static_fn().get_instance_proc_addr);
            vkc.inst = std::mem::transmute(self.ash_instance.handle());
            vkc.phys_dev = std::mem::transmute(self.phys);
            vkc.act_dev = std::mem::transmute(self.ash_device.handle());
            std::mem::forget(entry); // keep libvulkan loaded

            // Tell FFmpeg which features are enabled. We report what the
            // physical device supports for the 1.1/1.2/1.3 blocks that wgpu
            // enables as a whole is NOT guaranteed, so only claim the ones we
            // know are on: timeline semaphores (wgpu), sync2 (our callback),
            // sampler ycbcr is not enabled by wgpu.
            let f13: &'static mut vk::PhysicalDeviceVulkan13Features =
                Box::leak(Box::new(vk::PhysicalDeviceVulkan13Features::default().synchronization2(true)));
            let f12: &'static mut vk::PhysicalDeviceVulkan12Features =
                Box::leak(Box::new(vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true)));
            f12.p_next = f13 as *mut _ as *mut c_void;
            let f2: &mut vk::PhysicalDeviceFeatures2 = std::mem::transmute(&mut vkc.device_features);
            *f2 = vk::PhysicalDeviceFeatures2::default();
            f2.p_next = f12 as *mut _ as *mut c_void;

            let ie: &'static [*const c_char] =
                Box::leak(self.inst_exts.iter().map(|e| e.as_ptr()).collect::<Vec<_>>().into_boxed_slice());
            let de: &'static [*const c_char] =
                Box::leak(self.dev_exts.iter().map(|e| e.as_ptr()).collect::<Vec<_>>().into_boxed_slice());
            vkc.enabled_inst_extensions = ie.as_ptr();
            vkc.nb_enabled_inst_extensions = ie.len() as i32;
            vkc.enabled_dev_extensions = de.as_ptr();
            vkc.nb_enabled_dev_extensions = de.len() as i32;

            let gfx_flags = (vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER).as_raw();
            vkc.qf[0] = AVVulkanDeviceQueueFamily {
                idx: self.gfx_qf as i32,
                num: 1,
                flags: gfx_flags,
                video_caps: 0,
            };
            vkc.nb_qf = 1;
            if let Some((idx, flags, caps)) = self.dec_qf {
                vkc.qf[1] = AVVulkanDeviceQueueFamily {
                    idx: idx as i32,
                    num: 1,
                    flags,
                    video_caps: caps as i32,
                };
                vkc.nb_qf = 2;
            }
            let e = av_hwdevice_ctx_init(r);
            assert!(e >= 0, "av_hwdevice_ctx_init(vulkan on wgpu device): {}", err_str(e));
            HwDevice(r)
        }
    }

    fn next_cmd(&self) -> vk::CommandBuffer {
        let i = self.cmd_next.get();
        self.cmd_next.set((i + 1) % self.cmd_ring.len());
        self.cmd_ring[i]
    }
}

pub fn new_instance() -> wgpu::Instance {
    let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
    d.backends = wgpu::Backends::VULKAN;
    d.flags = wgpu::InstanceFlags::empty();
    wgpu::Instance::new(d)
}

// ---------------------------------------------------------------------------
// YUV -> RGB renderer

const SHADER: &str = r#"
struct U { scale: f32, planar: u32, uvs: vec2<f32> };
@group(0) @binding(0) var ty: texture_2d<f32>;
@group(0) @binding(1) var tuv: texture_2d<f32>;
@group(0) @binding(2) var tv: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<uniform> u: U;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    var o: VO;
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y) * u.uvs;
    return o;
}
// BT.709, limited ("tv") range -> non-linear R'G'B'. (No transfer function /
// chroma siting handling: this spike measures cost, colour is spec 05's job.)
@fragment fn fs(i: VO) -> @location(0) vec4<f32> {
    let y = textureSample(ty, samp, i.uv).r * u.scale;
    var c = textureSample(tuv, samp, i.uv).rg;
    let v2 = textureSample(tv, samp, i.uv).r;
    if (u.planar == 1u) { c = vec2<f32>(c.x, v2); }
    c = c * u.scale;
    let yy = (y - 16.0 / 255.0) * (255.0 / 219.0);
    let cb = (c.x - 128.0 / 255.0) * (255.0 / 224.0);
    let cr = (c.y - 128.0 / 255.0) * (255.0 / 224.0);
    return vec4<f32>(yy + 1.5748 * cr, yy - 0.1873 * cb - 0.4681 * cr, yy + 1.8556 * cb, 1.0);
}
"#;

pub struct Renderer {
    pub pipeline: wgpu::RenderPipeline,
    pub bgl: wgpu::BindGroupLayout,
    pub sampler: wgpu::Sampler,
    pub dummy: wgpu::TextureView,
    ubufs: HashMap<(u32, u32, u32, u32), wgpu::Buffer>,
}

impl Renderer {
    pub fn new(gpu: &Gpu, target: wgpu::TextureFormat) -> Renderer {
        let d = &gpu.device;
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let tex = |b| wgpu::BindGroupLayoutEntry {
            binding: b,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                tex(0),
                tex(1),
                tex(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(target.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = d.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy = d
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Renderer { pipeline, bgl, sampler, dummy, ubufs: HashMap::new() }
    }

    /// scale: multiplier applied to sampled values (bit-depth packing); planar: separate U/V textures
    pub fn bind(
        &mut self,
        gpu: &Gpu,
        y: &wgpu::TextureView,
        uv: &wgpu::TextureView,
        v: Option<&wgpu::TextureView>,
        scale: f32,
        uvs: [f32; 2],
    ) -> wgpu::BindGroup {
        let planar = v.is_some() as u32;
        let ubuf = self.ubufs.entry((scale.to_bits(), planar, uvs[0].to_bits(), uvs[1].to_bits())).or_insert_with(|| {
            let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut data = [0u8; 16];
            data[0..4].copy_from_slice(&scale.to_le_bytes());
            data[4..8].copy_from_slice(&planar.to_le_bytes());
            data[8..12].copy_from_slice(&uvs[0].to_le_bytes());
            data[12..16].copy_from_slice(&uvs[1].to_le_bytes());
            gpu.queue.write_buffer(&b, 0, &data);
            b
        });
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(y) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(uv) },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(v.unwrap_or(&self.dummy)),
                },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: ubuf.as_entire_binding() },
            ],
        })
    }

    pub fn draw(&self, enc: &mut wgpu::CommandEncoder, target: &wgpu::TextureView, bg: &wgpu::BindGroup) {
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(&self.pipeline);
        rp.set_bind_group(0, bg, &[]);
        rp.draw(0..3, 0..1);
    }
}

// ---------------------------------------------------------------------------
// Import paths

/// A frame imported into wgpu: plane views + sample scale. Holds whatever must
/// stay alive while the GPU reads it.
pub struct Imported {
    pub y: wgpu::TextureView,
    pub uv: wgpu::TextureView,
    pub v: Option<wgpu::TextureView>,
    pub scale: f32,
    /// fraction of the texture that is picture (Vulkan decode images have the coded, 16-aligned size)
    pub uvs: [f32; 2],
    _tex: Vec<wgpu::Texture>,
}

pub struct FrameGuard(pub *mut AVFrame);
unsafe impl Send for FrameGuard {}
unsafe impl Sync for FrameGuard {}
impl Drop for FrameGuard {
    fn drop(&mut self) {
        unsafe { av_frame_free(&mut self.0) }
    }
}

fn plane_formats(sw: AVPixelFormat) -> (wgpu::TextureFormat, wgpu::TextureFormat, wgpu::TextureFormat, f32) {
    use wgpu::TextureFormat as F;
    match sw {
        AVPixelFormat::AV_PIX_FMT_NV12 => (F::NV12, F::R8Unorm, F::Rg8Unorm, 1.0),
        // P010: 10 significant bits stored in the high bits of 16
        AVPixelFormat::AV_PIX_FMT_P010LE => (F::P010, F::R16Unorm, F::Rg16Unorm, 65535.0 / 65472.0),
        other => panic!("unsupported hw sw_format {:?}", other),
    }
}

pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;

pub fn describe_drm(desc: &AVDRMFrameDescriptor) -> String {
    let mut s = format!("objects={}", desc.nb_objects);
    for o in &desc.objects[..desc.nb_objects as usize] {
        s += &format!(" [size={} modifier={:#x}]", o.size, o.format_modifier);
    }
    s += &format!(" layers={}", desc.nb_layers);
    for l in &desc.layers[..desc.nb_layers as usize] {
        let f = l.format.to_le_bytes();
        s += &format!(" {{fourcc={} planes=", String::from_utf8_lossy(&f));
        for p in &l.planes[..l.nb_planes as usize] {
            s += &format!("(obj={} off={} pitch={})", p.object_index, p.offset, p.pitch);
        }
        s += "}";
    }
    s
}

/// Path (a): VAAPI surface -> DRM PRIME -> one single-plane Vulkan image per
/// plane via wgpu-hal's `texture_from_dmabuf_fd` (no raw ash needed).
/// The VAAPI frame must stay referenced while the textures are in use.
pub fn import_dmabuf(gpu: &Gpu, src: *mut AVFrame, verbose: bool) -> Imported {
    unsafe {
        assert!((*src).format == AVPixelFormat::AV_PIX_FMT_VAAPI as i32, "not a VAAPI frame (software fallback?)");
        let sw_format = (*((*(*src).hw_frames_ctx).data as *mut AVHWFramesContext)).sw_format;
        let (_, fy, fuv, scale) = plane_formats(sw_format);
        let drm = av_frame_alloc();
        (*drm).format = AVPixelFormat::AV_PIX_FMT_DRM_PRIME as i32;
        let e = av_hwframe_map(drm, src, AV_HWFRAME_MAP_READ as i32);
        assert!(e >= 0, "av_hwframe_map(vaapi->drm_prime): {}", err_str(e));
        let desc = &*((*drm).data[0] as *const AVDRMFrameDescriptor);
        if verbose {
            eprintln!("DRM PRIME descriptor: {}", describe_drm(desc));
        }
        // flatten planes in order (Y, UV)
        let mut planes = Vec::new();
        for l in &desc.layers[..desc.nb_layers as usize] {
            for p in &l.planes[..l.nb_planes as usize] {
                planes.push(*p);
            }
        }
        assert!(planes.len() == 2, "expected 2 planes (NV12/P010), got {}", planes.len());
        let (w, h) = ((*src).width as u32, (*src).height as u32);
        let hal_dev = gpu.device.as_hal::<Vulkan>().unwrap();
        let mut tex = Vec::new();
        for (i, p) in planes.iter().enumerate() {
            let obj = &desc.objects[p.object_index as usize];
            let (pw, ph, fmt) = if i == 0 { (w, h, fy) } else { (w.div_ceil(2), h.div_ceil(2), fuv) };
            let size = wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 };
            let fd = OwnedFd::from_raw_fd(libc::dup(obj.fd));
            let hal_tex = hal_dev
                .texture_from_dmabuf_fd(
                    fd,
                    &wgpu::hal::TextureDescriptor {
                        label: None,
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: fmt,
                        usage: wgpu::TextureUses::RESOURCE,
                        memory_flags: wgpu::hal::MemoryFlags::empty(),
                        view_formats: vec![],
                    },
                    obj.format_modifier,
                    p.pitch as u64,
                    p.offset as u64,
                )
                .unwrap_or_else(|e| panic!("texture_from_dmabuf_fd plane {i} ({fmt:?}, modifier {:#x}): {e:?}", obj.format_modifier));
            let t = gpu.device.create_texture_from_hal::<Vulkan>(
                hal_tex,
                &wgpu::TextureDescriptor {
                    label: None,
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: fmt,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                // Imported image is in VK_IMAGE_LAYOUT_UNDEFINED; wgpu will
                // transition UNDEFINED -> SHADER_READ_ONLY on first use.
                wgpu::TextureUses::UNINITIALIZED,
            );
            tex.push(t);
        }
        let mut drm = drm;
        av_frame_free(&mut drm); // closes the exported fds; we hold dups inside Vulkan
        Imported {
            y: tex[0].create_view(&Default::default()),
            uv: tex[1].create_view(&Default::default()),
            v: None,
            scale,
            uvs: [1.0, 1.0],
            _tex: tex,
        }
    }
}

/// Path (b): frame decoded by FFmpeg's Vulkan hwaccel on wgpu's own VkDevice.
/// Wraps the decoder's multi-planar VkImage as a wgpu NV12/P010 texture.
pub fn import_vkframe(gpu: &Gpu, src: *mut AVFrame, verbose: bool) -> Imported {
    unsafe {
        assert!((*src).format == AVPixelFormat::AV_PIX_FMT_VULKAN as i32);
        let sw_format = (*((*(*src).hw_frames_ctx).data as *mut AVHWFramesContext)).sw_format;
        let (fmt, fy, fuv, scale) = plane_formats(sw_format);
        let fc = (*(*src).hw_frames_ctx).data as *mut AVHWFramesContext;
        let vfc = &*((*fc).hwctx as *const AVVulkanFramesContext);
        let vkf = &mut *((*src).data[0] as *mut AVVkFrame);
        if verbose {
            eprintln!(
                "AVVkFrame: img={:#x},{:#x} layout={} sem_value={} qf={:#x} | frames ctx: vkformat={} usage={:#x} img_flags={:#x} tiling={} nb_layers={}",
                vkf.img[0], vkf.img[1], vkf.layout[0], vkf.sem_value[0], vkf.queue_family[0],
                vfc.format[0], vfc.usage as u32, vfc.img_flags, vfc.tiling, vfc.nb_layers
            );
        }
        assert!(vkf.img[1] == 0, "expected a single multi-planar image");
        (vfc.lock_frame.unwrap())(fc, vkf);
        let image = vk::Image::from_raw(vkf.img[0]);
        let sem = vk::Semaphore::from_raw(vkf.sem[0]);
        let wait_value = vkf.sem_value[0];
        let old_layout = vk::ImageLayout::from_raw(vkf.layout[0]);

        // 1) raw ash: transition decode layout -> SHADER_READ_ONLY on the
        //    graphics queue, gated on the decode's timeline semaphore.
        let cb = gpu.next_cmd();
        let dv = &gpu.ash_device;
        dv.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()).unwrap();
        dv.begin_command_buffer(
            cb,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )
        .unwrap();
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(old_layout)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        dv.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
        dv.end_command_buffer(cb).unwrap();
        let waits = [sem];
        let wait_vals = [wait_value];
        let sig_vals = [wait_value + 1];
        let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
        let cbs = [cb];
        let mut tl = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_vals)
            .signal_semaphore_values(&sig_vals);
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cbs)
            .signal_semaphores(&waits)
            .push_next(&mut tl);
        dv.queue_submit(gpu.raw_queue, &[submit], vk::Fence::null()).unwrap();

        // 2) the wgpu submission that samples the image waits for the barrier
        //    and signals the next timeline value so FFmpeg can reuse the frame
        //    (e.g. as a reference) only after we are done reading.
        {
            let hq = gpu.queue.as_hal::<Vulkan>().unwrap();
            hq.add_wait_semaphore(sem, Some(wait_value + 1), vk::PipelineStageFlags::FRAGMENT_SHADER);
            hq.add_signal_semaphore(sem, Some(wait_value + 2));
        }
        vkf.sem_value[0] = wait_value + 2;
        vkf.layout[0] = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL.as_raw();
        vkf.access[0] = vk::AccessFlags::SHADER_READ.as_raw() as u64;
        (vfc.unlock_frame.unwrap())(fc, vkf);

        // The VkImage has the coded size (aligned to 16: 1080p is 1920x1088); crop via UVs.
        let (pw, ph) = ((*src).width as u32, (*src).height as u32);
        let (w, h) = ((*fc).width as u32, (*fc).height as u32);
        if verbose && (pw, ph) != (w, h) {
            eprintln!("vulkan image {w}x{h} larger than picture {pw}x{ph}: cropping with UV scale");
        }
        let size = wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 };
        let guard = FrameGuard(av_frame_clone(src));
        let hal_dev = gpu.device.as_hal::<Vulkan>().unwrap();
        let hal_tex = hal_dev.texture_from_raw(
            image,
            &wgpu::hal::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            },
            Some(Box::new(move || drop(guard))),
            wgpu::hal::vulkan::TextureMemory::External,
        );
        let t = gpu.device.create_texture_from_hal::<Vulkan>(
            hal_tex,
            &wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::RESOURCE, // == SHADER_READ_ONLY_OPTIMAL after our barrier
        );
        let view = |aspect, format| {
            t.create_view(&wgpu::TextureViewDescriptor {
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        Imported {
            y: view(wgpu::TextureAspect::Plane0, fy),
            uv: view(wgpu::TextureAspect::Plane1, fuv),
            v: None,
            scale,
            uvs: [pw as f32 / w as f32, ph as f32 / h as f32],
            _tex: vec![t],
        }
    }
}

/// Path (c): CPU frame -> persistent textures via queue.write_texture.
pub struct CpuUploader {
    tex: Vec<wgpu::Texture>,
    views: Vec<wgpu::TextureView>,
    fmt: AVPixelFormat,
}

impl CpuUploader {
    pub fn new() -> Self {
        CpuUploader { tex: vec![], views: vec![], fmt: AVPixelFormat::AV_PIX_FMT_NONE }
    }
    pub fn upload(&mut self, gpu: &Gpu, f: *const AVFrame) -> Imported {
        use wgpu::TextureFormat as F;
        unsafe {
            let fmt: AVPixelFormat = std::mem::transmute((*f).format);
            let (w, h) = ((*f).width as u32, (*f).height as u32);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            // (format, width, height, bytes per texel)
            let (planes, scale): (Vec<(F, u32, u32, u32)>, f32) = match fmt {
                AVPixelFormat::AV_PIX_FMT_NV12 => (vec![(F::R8Unorm, w, h, 1), (F::Rg8Unorm, cw, ch, 2)], 1.0),
                AVPixelFormat::AV_PIX_FMT_P010LE => {
                    (vec![(F::R16Unorm, w, h, 2), (F::Rg16Unorm, cw, ch, 4)], 65535.0 / 65472.0)
                }
                AVPixelFormat::AV_PIX_FMT_YUV420P | AVPixelFormat::AV_PIX_FMT_YUVJ420P => {
                    (vec![(F::R8Unorm, w, h, 1), (F::R8Unorm, cw, ch, 1), (F::R8Unorm, cw, ch, 1)], 1.0)
                }
                AVPixelFormat::AV_PIX_FMT_YUV420P10LE => (
                    vec![(F::R16Unorm, w, h, 2), (F::R16Unorm, cw, ch, 2), (F::R16Unorm, cw, ch, 2)],
                    65535.0 / 1023.0,
                ),
                other => panic!("cpu upload: unsupported pix fmt {:?}", other),
            };
            if self.fmt != fmt {
                self.fmt = fmt;
                self.tex = planes
                    .iter()
                    .map(|&(format, pw, ph, _)| {
                        gpu.device.create_texture(&wgpu::TextureDescriptor {
                            label: None,
                            size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        })
                    })
                    .collect();
                self.views = self.tex.iter().map(|t| t.create_view(&Default::default())).collect();
            }
            for (i, &(_, pw, ph, bpt)) in planes.iter().enumerate() {
                let stride = (*f).linesize[i] as u32;
                let len = (stride * (ph - 1) + pw * bpt) as usize;
                let data = std::slice::from_raw_parts((*f).data[i], len);
                gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.tex[i],
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    data,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(stride), rows_per_image: Some(ph) },
                    wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
                );
            }
            Imported {
                y: self.views[0].clone(),
                uv: self.views[1].clone(),
                v: self.views.get(2).cloned(),
                scale,
                uvs: [1.0, 1.0],
                _tex: vec![],
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Path {
    Dmabuf,
    DmabufNoCache,
    VkDec,
    CpuVaapi,
    CpuVulkan,
    CpuSw,
}

impl Path {
    pub fn parse(s: &str) -> Path {
        match s {
            "dmabuf" => Path::Dmabuf,
            "dmabuf-nocache" => Path::DmabufNoCache,
            "vkdec" => Path::VkDec,
            "cpu-vaapi" => Path::CpuVaapi,
            "cpu-vulkan" => Path::CpuVulkan,
            "cpu-sw" => Path::CpuSw,
            _ => panic!("path must be dmabuf|dmabuf-nocache|vkdec|cpu-vaapi|cpu-vulkan|cpu-sw"),
        }
    }
    pub fn hw(self) -> Hw {
        match self {
            Path::Dmabuf | Path::DmabufNoCache | Path::CpuVaapi => Hw::Vaapi,
            Path::VkDec | Path::CpuVulkan => Hw::Vulkan,
            Path::CpuSw => Hw::Sw,
        }
    }
    pub fn needs_video_device(self) -> bool {
        self == Path::VkDec
    }
}

/// Everything needed to turn decoded frames into bind groups for one stream.
pub struct Importer {
    pub path: Path,
    cache: HashMap<u32, (std::rc::Rc<Imported>, wgpu::BindGroup)>,
    uploader: CpuUploader,
    first: bool,
    /// frame references that must outlive in-flight GPU work (dmabuf paths)
    hold: std::collections::VecDeque<FrameGuard>,
}

impl Importer {
    pub fn new(path: Path) -> Self {
        Importer { path, cache: HashMap::new(), uploader: CpuUploader::new(), first: true, hold: Default::default() }
    }

    pub fn open_decoder(&self, gpu: &Gpu, file: &str) -> (Decoder, Option<HwDevice>) {
        let dev = match self.path {
            Path::VkDec => Some(gpu.ffmpeg_vulkan_device()),
            Path::CpuSw => None,
            p => Some(HwDevice::create(p.hw())),
        };
        (Decoder::open(file, self.path.hw(), dev.as_ref(), 0), dev)
    }

    /// Import `d.frame` and return a bind group for the renderer.
    pub fn import(&mut self, gpu: &Gpu, r: &mut Renderer, d: &mut Decoder) -> wgpu::BindGroup {
        match self.path {
            Path::Dmabuf | Path::DmabufNoCache => d.sync_vaapi(),
            Path::CpuVaapi | Path::CpuVulkan | Path::CpuSw => {
                let f = d.download();
                return self.import_frame(gpu, r, f);
            }
            _ => {}
        }
        self.import_frame(gpu, r, d.frame)
    }

    /// Import an already synced (VAAPI) / downloaded (CPU paths) frame.
    pub fn import_frame(&mut self, gpu: &Gpu, r: &mut Renderer, frame: *mut AVFrame) -> wgpu::BindGroup {
        let verbose = std::mem::replace(&mut self.first, false);
        match self.path {
            Path::Dmabuf => {
                // Vulkan has no implicit sync with VAAPI: wait for the decode.

                self.hold_frame(frame);
                let id = unsafe { (*frame).data[3] as usize as u32 };
                if let Some((_, bg)) = self.cache.get(&id) {
                    return bg.clone();
                }
                let imp = import_dmabuf(gpu, frame, verbose);
                let bg = r.bind(gpu, &imp.y, &imp.uv, None, imp.scale, imp.uvs);
                self.cache.insert(id, (std::rc::Rc::new(imp), bg.clone()));
                bg
            }
            Path::DmabufNoCache => {

                self.hold_frame(frame);
                let imp = import_dmabuf(gpu, frame, verbose);
                r.bind(gpu, &imp.y, &imp.uv, None, imp.scale, imp.uvs)
            }
            Path::VkDec => {
                let imp = import_vkframe(gpu, frame, verbose);
                r.bind(gpu, &imp.y, &imp.uv, None, imp.scale, imp.uvs)
            }
            Path::CpuVaapi | Path::CpuVulkan | Path::CpuSw => {
                let f = frame;
                let imp = self.uploader.upload(gpu, f);
                r.bind(gpu, &imp.y, &imp.uv, imp.v.as_ref(), imp.scale, imp.uvs)
            }
        }
    }

    fn hold_frame(&mut self, frame: *mut AVFrame) {
        unsafe { self.hold.push_back(FrameGuard(av_frame_clone(frame))) };
        // callers poll the device at least every 4 frames; 12 is conservative
        while self.hold.len() > 12 {
            self.hold.pop_front();
        }
    }

    pub fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

fn wait_idle(gpu: &Gpu) {
    gpu.device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .unwrap();
}

fn make_target(gpu: &Gpu, w: u32, h: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// s1 import <file> <path> [frames=N]
/// decode -> import -> YUV->RGB into an RGBA16F target of the video's size.
pub fn cmd_import(args: &[String]) {
    let file = &args[2];
    let path = Path::parse(&args[3]);
    let max_frames: u64 = crate::arg_kv(args, "frames").map(|s| s.parse().unwrap()).unwrap_or(u64::MAX);
    let gpu = Gpu::headless(path.needs_video_device());
    let mut imp = Importer::new(path);
    let (mut d, _dev) = imp.open_decoder(&gpu, file);
    let mut r = Renderer::new(&gpu, wgpu::TextureFormat::Rgba16Float);
    let target = make_target(&gpu, d.width, d.height, wgpu::TextureFormat::Rgba16Float);
    let tv = target.create_view(&Default::default());

    let (mut t_dec, mut t_imp, mut t_sub) = (0f64, 0f64, 0f64);
    let mut per_frame = Vec::new();
    let cpu0 = cpu_secs();
    let t0 = Instant::now();
    let mut n = 0u64;
    let mut fmt_name = String::new();
    loop {
        let a = Instant::now();
        if n >= max_frames || !d.next_frame() {
            break;
        }
        let b = Instant::now();
        if n == 0 {
            fmt_name = d.frame_format();
        }
        let bg = imp.import(&gpu, &mut r, &mut d);
        let c = Instant::now();
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.draw(&mut enc, &tv, &bg);
        gpu.queue.submit([enc.finish()]);
        n += 1;
        if n % 4 == 0 {
            // bound the amount of in-flight GPU work so fps is real throughput
            wait_idle(&gpu);
        }
        let e = Instant::now();
        t_dec += (b - a).as_secs_f64();
        t_imp += (c - b).as_secs_f64();
        t_sub += (e - c).as_secs_f64();
        per_frame.push((e - a).as_secs_f64() * 1000.0);
    }
    wait_idle(&gpu);
    let dt = t0.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    per_frame.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let ms = |t: f64| t * 1000.0 / n as f64;
    println!(
        "import file={} path={:?} frames={} time={:.3}s fps={:.1} ms/frame={:.3} [decode={:.3} import={:.3} render+submit+wait={:.3}] p50={:.2} p99={:.2} max={:.2} cpu/frame={:.3}ms fmt={} cached_surfaces={}",
        crate::file_label(file),
        path,
        n,
        dt,
        n as f64 / dt,
        ms(dt),
        ms(t_dec),
        ms(t_imp),
        ms(t_sub),
        percentile(&per_frame, 0.5),
        percentile(&per_frame, 0.99),
        per_frame.last().copied().unwrap_or(f64::NAN),
        cpu * 1000.0 / n as f64,
        fmt_name,
        imp.cache_len(),
    );
    drop(imp);
    drop(d);
    wait_idle(&gpu);
}

fn render_frame_rgba8(file: &str, path: Path, index: u64) -> Vec<u8> {
    let gpu = Gpu::headless(path.needs_video_device());
    let mut imp = Importer::new(path);
    let (mut d, _dev) = imp.open_decoder(&gpu, file);
    let mut r = Renderer::new(&gpu, wgpu::TextureFormat::Rgba8Unorm);
    let (w, h) = (d.width, d.height);
    let target = make_target(&gpu, w, h, wgpu::TextureFormat::Rgba8Unorm);
    let tv = target.create_view(&Default::default());
    for _ in 0..=index {
        assert!(d.next_frame());
    }
    let bg = imp.import(&gpu, &mut r, &mut d);
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    r.draw(&mut enc, &tv, &bg);
    let bpr = (w * 4).div_ceil(256) * 256;
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (bpr * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    gpu.queue.submit([enc.finish()]);
    buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.unwrap());
    wait_idle(&gpu);
    let data = buf.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        let s = (y * bpr) as usize;
        out.extend_from_slice(&data[s..s + (w * 4) as usize]);
    }
    drop(data);
    drop(imp);
    drop(d);
    wait_idle(&gpu);
    out
}

/// s1 verify <file> <frame-index>: render the same frame through every path
/// and compare the RGBA8 result against the software-decode path.
pub fn cmd_verify(args: &[String]) {
    let file = &args[2];
    let index: u64 = args[3].parse().unwrap();
    let reference = render_frame_rgba8(file, Path::CpuSw, index);
    let mean: f64 = reference.iter().map(|&b| b as f64).sum::<f64>() / reference.len() as f64;
    println!("verify file={} frame={} reference=cpu-sw mean_value={:.2}", crate::file_label(file), index, mean);
    let only = crate::arg_kv(args, "only").map(|s| Path::parse(&s));
    // The Vulkan decode paths can oops the amdgpu kernel driver (the calling thread dies in the
    // ioctl and the process hangs): they only run with S1_VULKAN=1.
    for p in [Path::Dmabuf, Path::VkDec, Path::CpuVaapi, Path::CpuVulkan] {
        if (p == Path::CpuVulkan || p == Path::VkDec) && !crate::vulkan_allowed() {
            println!("  {:?}: skipped (set S1_VULKAN=1)", p);
            continue;
        }
        if only.is_none() && p == Path::CpuVulkan {
            continue;
        }
        if only.is_some_and(|o| o != p) {
            continue;
        }
        let out = std::panic::catch_unwind(|| render_frame_rgba8(file, p, index));
        match out {
            Ok(out) => {
                let mut max = 0u8;
                let mut sum = 0u64;
                let mut differing = 0u64;
                for (a, b) in out.iter().zip(reference.iter()) {
                    let d = a.abs_diff(*b);
                    max = max.max(d);
                    sum += d as u64;
                    differing += (d != 0) as u64;
                }
                println!(
                    "  {:?}: max_abs_diff={} mean_abs_diff={:.5} differing_bytes={:.4}%",
                    p,
                    max,
                    sum as f64 / out.len() as f64,
                    differing as f64 * 100.0 / out.len() as f64
                );
            }
            Err(_) => println!("  {:?}: FAILED (see panic above)", p),
        }
    }
}
