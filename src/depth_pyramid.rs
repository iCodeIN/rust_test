const SIMPLE_FRAGMENT_SHADER: bool = false;
const CUBE_BACKFACE_OPTIMIZATION: bool = true;
const NUM_INSTANCES: usize = 1024 * 1024;

use std::default::Default;
use std::ffi::CString;
use std::io::Cursor;
use std::mem;

use ash::util::*;
use ash::{vk, Device};

use crate::vulkan_base::*;
use crate::vulkan_helpers::*;

#[derive(Clone, Copy)]
pub struct DepthPyramidUniforms {
    pub dimensions: u32, // pow2 y dimension of mip 0 (texture x is 1.5x wider)
    pub mip: u32,
}

#[derive(Clone, Copy)]
pub struct DepthPyramidMip0Uniforms {
    pub src_dimensions: (u32, u32),
    pub dst_dimensions: u32, // pow2 y dimension of mip 0 (texture x is 1.5x wider)
}

pub struct DepthPyramid {
    pub pipeline_layout: vk::PipelineLayout,
    pub uniform_buffer: VkBuffer,
    pub uniform_buffer_gpu: VkBuffer,
    pub image: VkImage,
    pub sampler: vk::Sampler,
    pub view: vk::ImageView,
    pub descriptor: vk::DescriptorImageInfo,
    pub desc_set_layout: vk::DescriptorSetLayout,
    pub compute_pipeline: vk::Pipeline,
    pub descriptor_sets: Vec<vk::DescriptorSet>,
    pub compute_shader_module: vk::ShaderModule,
}

impl DepthPyramid {
    pub fn new(
        device: &Device,
        allocator: &vk_mem::Allocator,
        descriptor_pool: &vk::DescriptorPool,
        depth_view: &vk::ImageView,
        image_dimensions: (u32, u32),
    ) -> DepthPyramid {
        let alloc_info_cpu = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::CpuOnly,
            flags: vk_mem::AllocationCreateFlags::MAPPED,
            ..Default::default()
        };

        let alloc_info_gpu = vk_mem::AllocationCreateInfo {
            usage: vk_mem::MemoryUsage::GpuOnly,
            ..Default::default()
        };

        let uniform_buffer_info = vk::BufferCreateInfo {
            size: std::mem::size_of::<DepthPyramidMip0Uniforms>() as u64,
            usage: vk::BufferUsageFlags::TRANSFER_SRC,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            ..Default::default()
        };

        let uniform_buffer = VkBuffer::new(allocator, &uniform_buffer_info, &alloc_info_cpu);

        let uniform_buffer_gpu_info = vk::BufferCreateInfo {
            size: std::mem::size_of::<DepthPyramidMip0Uniforms>() as u64,
            usage: vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::UNIFORM_BUFFER,
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            ..Default::default()
        };

        let uniform_buffer_gpu =
            VkBuffer::new(allocator, &uniform_buffer_gpu_info, &alloc_info_gpu);

        let texture_create_info = vk::ImageCreateInfo {
            image_type: vk::ImageType::TYPE_2D,
            format: vk::Format::R32_SFLOAT,
            extent: vk::Extent3D {
                width: image_dimensions.0,
                height: image_dimensions.1,
                depth: 1,
            },
            mip_levels: 1,
            array_layers: 1,
            samples: vk::SampleCountFlags::TYPE_1,
            tiling: vk::ImageTiling::OPTIMAL,
            usage: vk::ImageUsageFlags::STORAGE, /* | vk::ImageUsageFlags::SAMPLED*/
            sharing_mode: vk::SharingMode::EXCLUSIVE,
            ..Default::default()
        };

        let image = VkImage::new(&allocator, &texture_create_info, &alloc_info_gpu);

        let sampler_info = vk::SamplerCreateInfo {
            mag_filter: vk::Filter::LINEAR,
            min_filter: vk::Filter::LINEAR,
            mipmap_mode: vk::SamplerMipmapMode::NEAREST,
            address_mode_u: vk::SamplerAddressMode::REPEAT,
            address_mode_v: vk::SamplerAddressMode::REPEAT,
            address_mode_w: vk::SamplerAddressMode::REPEAT,
            max_anisotropy: 1.0,
            border_color: vk::BorderColor::FLOAT_OPAQUE_WHITE,
            compare_op: vk::CompareOp::NEVER,
            ..Default::default()
        };

        let sampler = unsafe { device.create_sampler(&sampler_info, None).unwrap() };

        let view_info = vk::ImageViewCreateInfo {
            view_type: vk::ImageViewType::TYPE_2D,
            format: texture_create_info.format,
            components: vk::ComponentMapping {
                r: vk::ComponentSwizzle::R,
                g: vk::ComponentSwizzle::G,
                b: vk::ComponentSwizzle::B,
                a: vk::ComponentSwizzle::A,
            },
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                level_count: 1,
                layer_count: 1,
                ..Default::default()
            },
            image: image.image,
            ..Default::default()
        };
        let view = unsafe { device.create_image_view(&view_info, None) }.unwrap();

        let descriptor = vk::DescriptorImageInfo {
            image_layout: vk::ImageLayout::GENERAL,
            image_view: view,
            ..Default::default() //sampler,
        };

        let desc_layout_bindings = [
            vk::DescriptorSetLayoutBinding {
                binding: 0,
                descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                ..Default::default()
            },
            vk::DescriptorSetLayoutBinding {
                binding: 1,
                descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                ..Default::default()
            },
            vk::DescriptorSetLayoutBinding {
                binding: 2,
                descriptor_type: vk::DescriptorType::STORAGE_IMAGE,
                descriptor_count: 1,
                stage_flags: vk::ShaderStageFlags::COMPUTE,
                ..Default::default()
            },
        ];
        let descriptor_info =
            vk::DescriptorSetLayoutCreateInfo::builder().bindings(&desc_layout_bindings);

        let desc_set_layout =
            unsafe { device.create_descriptor_set_layout(&descriptor_info, None) }.unwrap();

        let desc_set_layouts = &[desc_set_layout];

        let descriptor_sets = {
            let desc_alloc_info = vk::DescriptorSetAllocateInfo::builder()
                .descriptor_pool(*descriptor_pool)
                .set_layouts(desc_set_layouts);

            unsafe { device.allocate_descriptor_sets(&desc_alloc_info) }.unwrap()
        };

        let uniform_buffer_descriptor = vk::DescriptorBufferInfo {
            buffer: uniform_buffer_gpu.buffer,
            offset: 0,
            range: mem::size_of::<DepthPyramidMip0Uniforms>() as u64,
        };

        let depth_image_descriptor = vk::DescriptorImageInfo {
            image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            image_view: *depth_view,
            sampler,
        };

        let write_desc_sets = [
            vk::WriteDescriptorSet {
                dst_set: descriptor_sets[0],
                dst_binding: 0,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::UNIFORM_BUFFER,
                p_buffer_info: &uniform_buffer_descriptor,
                ..Default::default()
            },
            vk::WriteDescriptorSet {
                dst_set: descriptor_sets[0],
                dst_binding: 1,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                p_image_info: &depth_image_descriptor,
                ..Default::default()
            },
            vk::WriteDescriptorSet {
                dst_set: descriptor_sets[0],
                dst_binding: 2,
                descriptor_count: 1,
                descriptor_type: vk::DescriptorType::STORAGE_IMAGE,
                p_image_info: &descriptor,
                ..Default::default()
            },
        ];
        unsafe { device.update_descriptor_sets(&write_desc_sets, &[]) };

        let layout_create_info =
            vk::PipelineLayoutCreateInfo::builder().set_layouts(desc_set_layouts);

        let pipeline_layout =
            unsafe { device.create_pipeline_layout(&layout_create_info, None) }.unwrap();

        let mut comp_spv_file =
            Cursor::new(&include_bytes!("../shader/depth_pyramid_first_mip.spv"));

        let comp_code =
            read_spv(&mut comp_spv_file).expect("Failed to read compute shader spv file");
        let comp_shader_info = vk::ShaderModuleCreateInfo::builder().code(&comp_code);

        let compute_shader_module = unsafe { device.create_shader_module(&comp_shader_info, None) }
            .expect("Fragment shader module error");

        let shader_entry_name = CString::new("main").unwrap();
        let shader_stage_create_info = vk::PipelineShaderStageCreateInfo {
            module: compute_shader_module,
            p_name: shader_entry_name.as_ptr(),
            stage: vk::ShaderStageFlags::COMPUTE,
            ..Default::default()
        };

        let compute_pipeline_infos = vk::ComputePipelineCreateInfo::builder()
            .stage(shader_stage_create_info)
            .layout(pipeline_layout);

        let compute_pipelines = unsafe {
            device.create_compute_pipelines(
                vk::PipelineCache::null(),
                &[compute_pipeline_infos.build()],
                None,
            )
        }
        .unwrap();

        let compute_pipeline = compute_pipelines[0];

        DepthPyramid {
            pipeline_layout,
            uniform_buffer,
            uniform_buffer_gpu,
            desc_set_layout,
            compute_pipeline,
            descriptor_sets,
            compute_shader_module,
            image,
            sampler,
            view,
            descriptor,
        }
    }

    pub fn gpu_setup(&self, device: &Device, command_buffer: &vk::CommandBuffer) {
        // Transition texture to read & write layout
        let texture_barrier = vk::ImageMemoryBarrier {
            dst_access_mask: vk::AccessFlags::SHADER_WRITE,
            new_layout: vk::ImageLayout::GENERAL,
            image: self.image.image,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                level_count: 1,
                layer_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };

        unsafe {
            device.cmd_pipeline_barrier(
                *command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[texture_barrier],
            );
        };
    }

    pub fn update(&self, uniforms: &DepthPyramidMip0Uniforms) {
        self.uniform_buffer.copy_from_slice(&[*uniforms], 0);
    }

    pub fn gpu_draw(
        &self,
        device: &Device,
        command_buffer: &vk::CommandBuffer,
        depth_image: &vk::Image,
        pyramid_mip0_dimension: u32,
    ) {
        let buffer_copy_regions = vk::BufferCopy {
            src_offset: 0,
            dst_offset: 0,
            size: self.uniform_buffer.size,
        };

        let buffer_barrier = vk::BufferMemoryBarrier {
            dst_access_mask: vk::AccessFlags::TRANSFER_WRITE,
            buffer: self.uniform_buffer_gpu.buffer,
            offset: 0,
            size: buffer_copy_regions.size,
            ..Default::default()
        };

        let buffer_barrier_end = vk::BufferMemoryBarrier {
            src_access_mask: vk::AccessFlags::TRANSFER_WRITE,
            dst_access_mask: vk::AccessFlags::INDEX_READ,
            buffer: self.uniform_buffer_gpu.buffer,
            offset: 0,
            size: buffer_copy_regions.size,
            ..Default::default()
        };

        let texture_barrier = vk::ImageMemoryBarrier {
            src_access_mask: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            dst_access_mask: vk::AccessFlags::SHADER_READ,
            old_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            image: *depth_image,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::DEPTH,
                level_count: 1,
                layer_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };

        let layout_transition_barriers = vk::ImageMemoryBarrier {
            src_access_mask: vk::AccessFlags::SHADER_READ,
            dst_access_mask: vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_READ
                | vk::AccessFlags::DEPTH_STENCIL_ATTACHMENT_WRITE,
            old_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            new_layout: vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            image: *depth_image,
            subresource_range: vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::DEPTH,
                level_count: 1,
                layer_count: 1,
                ..Default::default()
            },
            ..Default::default()
        };

        unsafe {
            // Update uniform buffer
            device.cmd_pipeline_barrier(
                *command_buffer,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[buffer_barrier],
                &[],
            );

            device.cmd_copy_buffer(
                *command_buffer,
                self.uniform_buffer.buffer,
                self.uniform_buffer_gpu.buffer,
                &[buffer_copy_regions],
            );

            device.cmd_pipeline_barrier(
                *command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::VERTEX_INPUT,
                vk::DependencyFlags::empty(),
                &[],
                &[buffer_barrier_end],
                &[],
            );

            // Transition depth buffer to read
            device.cmd_pipeline_barrier(
                *command_buffer,
                vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[texture_barrier],
            );

            // First pass downsample compute shader
            device.cmd_bind_pipeline(
                *command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.compute_pipeline,
            );

            device.cmd_bind_descriptor_sets(
                *command_buffer,
                vk::PipelineBindPoint::COMPUTE,
                self.pipeline_layout,
                0,
                &self.descriptor_sets[..],
                &[],
            );

            let group_dim = (8, 8);
            let dim = (
                pyramid_mip0_dimension / group_dim.0,
                pyramid_mip0_dimension / group_dim.1,
            );
            device.cmd_dispatch(*command_buffer, dim.0, dim.1, 1);

            // Transition depth buffer to depth attachment
            device.cmd_pipeline_barrier(
                *command_buffer,
                vk::PipelineStageFlags::COMPUTE_SHADER,
                vk::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[layout_transition_barriers],
            );
        }
    }

    pub fn destroy(&self, device: &Device, allocator: &vk_mem::Allocator) {
        unsafe {
            device.destroy_image_view(self.view, None);
            self.image.destroy(allocator);
            self.uniform_buffer.destroy(&allocator);
            self.uniform_buffer_gpu.destroy(&allocator);
            device.destroy_sampler(self.sampler, None);
            device.destroy_pipeline(self.compute_pipeline, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_shader_module(self.compute_shader_module, None);
            device.destroy_descriptor_set_layout(self.desc_set_layout, None);
        }
    }
}
