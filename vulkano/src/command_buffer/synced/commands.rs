// Copyright (c) 2016 The vulkano developers
// Licensed under the Apache License, Version 2.0
// <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT
// license <LICENSE-MIT or https://opensource.org/licenses/MIT>,
// at your option. All files in the project carrying such
// notice may not be copied, modified, or distributed except
// according to those terms.

use super::*;
use crate::buffer::BufferAccess;
use crate::buffer::TypedBufferAccess;
use crate::command_buffer::synced::builder::KeyTy;
use crate::command_buffer::synced::builder::SyncCommandBufferBuilder;
use crate::command_buffer::synced::builder::SyncCommandBufferBuilderError;
use crate::command_buffer::sys::UnsafeCommandBufferBuilder;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderBindVertexBuffer;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderBufferImageCopy;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderColorImageClear;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderDepthStencilImageClear;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderExecuteCommands;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderImageBlit;
use crate::command_buffer::sys::UnsafeCommandBufferBuilderImageCopy;
use crate::command_buffer::CommandBufferExecError;
use crate::command_buffer::ImageUninitializedSafe;
use crate::command_buffer::SecondaryCommandBuffer;
use crate::command_buffer::SubpassContents;
use crate::descriptor_set::layout::DescriptorDescTy;
use crate::descriptor_set::DescriptorSet;
use crate::descriptor_set::DescriptorSetWithOffsets;
use crate::format::ClearValue;
use crate::image::ImageAccess;
use crate::image::ImageLayout;
use crate::pipeline::depth_stencil::StencilFaces;
use crate::pipeline::input_assembly::IndexType;
use crate::pipeline::layout::PipelineLayout;
use crate::pipeline::shader::ShaderStages;
use crate::pipeline::vertex::VertexInput;
use crate::pipeline::viewport::Scissor;
use crate::pipeline::viewport::Viewport;
use crate::pipeline::ComputePipeline;
use crate::pipeline::GraphicsPipeline;
use crate::pipeline::PipelineBindPoint;
use crate::query::QueryControlFlags;
use crate::query::QueryPool;
use crate::query::QueryResultElement;
use crate::query::QueryResultFlags;
use crate::render_pass::FramebufferAbstract;
use crate::render_pass::LoadOp;
use crate::sampler::Filter;
use crate::sync::AccessFlags;
use crate::sync::Event;
use crate::sync::PipelineMemoryAccess;
use crate::sync::PipelineStage;
use crate::sync::PipelineStages;
use crate::DeviceSize;
use crate::SafeDeref;
use crate::VulkanObject;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::ffi::CStr;
use std::mem;
use std::ops::Range;
use std::ptr;
use std::sync::{Arc, Mutex};

impl SyncCommandBufferBuilder {
    /// Calls `vkCmdBeginQuery` on the builder.
    #[inline]
    pub unsafe fn begin_query(
        &mut self,
        query_pool: Arc<QueryPool>,
        query: u32,
        flags: QueryControlFlags,
    ) {
        struct Cmd {
            query_pool: Arc<QueryPool>,
            query: u32,
            flags: QueryControlFlags,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdBeginQuery"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.begin_query(self.query_pool.query(self.query).unwrap(), self.flags);
            }
        }

        self.append_command(
            Cmd {
                query_pool,
                query,
                flags,
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkBeginRenderPass` on the builder.
    // TODO: it shouldn't be possible to get an error if the framebuffer checked conflicts already
    // TODO: after begin_render_pass has been called, flushing should be forbidden and an error
    //       returned if conflict
    #[inline]
    pub unsafe fn begin_render_pass<F, I>(
        &mut self,
        framebuffer: F,
        subpass_contents: SubpassContents,
        clear_values: I,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        F: FramebufferAbstract + 'static,
        I: IntoIterator<Item = ClearValue> + Send + Sync + 'static,
    {
        struct Cmd<F, I> {
            framebuffer: F,
            subpass_contents: SubpassContents,
            clear_values: Mutex<Option<I>>,
        }

        impl<F, I> Command for Cmd<F, I>
        where
            F: FramebufferAbstract + 'static,
            I: IntoIterator<Item = ClearValue> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdBeginRenderPass"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.begin_render_pass(
                    &self.framebuffer,
                    self.subpass_contents,
                    self.clear_values.lock().unwrap().take().unwrap(),
                );
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                self.framebuffer.attached_image_view(num).unwrap().image()
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                format!("attachment {}", num).into()
            }
        }

        let resources = framebuffer
            .render_pass()
            .desc()
            .attachments()
            .iter()
            .map(|desc| {
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                all_commands: true,
                                ..PipelineStages::none()
                            }, // TODO: wrong!
                            access: AccessFlags {
                                input_attachment_read: true,
                                color_attachment_read: true,
                                color_attachment_write: true,
                                depth_stencil_attachment_read: true,
                                depth_stencil_attachment_write: true,
                                ..AccessFlags::none()
                            }, // TODO: suboptimal
                            exclusive: true, // TODO: suboptimal ; note: remember to always pass true if desc.initial_layout != desc.final_layout
                        },
                        desc.initial_layout,
                        desc.final_layout,
                        match desc.initial_layout != ImageLayout::Undefined
                            || desc.load == LoadOp::Clear
                        {
                            true => ImageUninitializedSafe::Safe,
                            false => ImageUninitializedSafe::Unsafe,
                        },
                    )),
                )
            })
            .collect::<Vec<_>>();

        self.append_command(
            Cmd {
                framebuffer,
                subpass_contents,
                clear_values: Mutex::new(Some(clear_values)),
            },
            &resources,
        )?;

        self.latest_render_pass_enter = Some(self.commands.len() - 1);
        Ok(())
    }

    /// Starts the process of binding descriptor sets. Returns an intermediate struct which can be
    /// used to add the sets.
    #[inline]
    pub fn bind_descriptor_sets(&mut self) -> SyncCommandBufferBuilderBindDescriptorSets {
        SyncCommandBufferBuilderBindDescriptorSets {
            builder: self,
            descriptor_sets: SmallVec::new(),
        }
    }

    /// Calls `vkCmdBindIndexBuffer` on the builder.
    #[inline]
    pub unsafe fn bind_index_buffer<B>(&mut self, buffer: B, index_ty: IndexType)
    where
        B: BufferAccess + 'static,
    {
        struct Cmd<B> {
            buffer: B,
            index_ty: IndexType,
        }

        impl<B> Command for Cmd<B>
        where
            B: BufferAccess + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdBindIndexBuffer"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.bind_index_buffer(&self.buffer, self.index_ty);
            }

            fn bound_index_buffer(&self) -> (&dyn BufferAccess, IndexType) {
                (&self.buffer, self.index_ty)
            }
        }

        self.append_command(Cmd { buffer, index_ty }, &[]).unwrap();
        self.current_state.index_buffer = self.commands.last().cloned();
    }

    /// Calls `vkCmdBindPipeline` on the builder with a compute pipeline.
    #[inline]
    pub unsafe fn bind_pipeline_compute(&mut self, pipeline: Arc<ComputePipeline>) {
        struct Cmd {
            pipeline: Arc<ComputePipeline>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdBindPipeline"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.bind_pipeline_compute(&self.pipeline);
            }

            fn bound_pipeline_compute(&self) -> &Arc<ComputePipeline> {
                &self.pipeline
            }
        }

        self.append_command(Cmd { pipeline }, &[]).unwrap();
        self.current_state.pipeline_compute = self.commands.last().cloned();
    }

    /// Calls `vkCmdBindPipeline` on the builder with a graphics pipeline.
    #[inline]
    pub unsafe fn bind_pipeline_graphics(&mut self, pipeline: Arc<GraphicsPipeline>) {
        struct Cmd {
            pipeline: Arc<GraphicsPipeline>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdBindPipeline"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.bind_pipeline_graphics(&self.pipeline);
            }

            fn bound_pipeline_graphics(&self) -> &Arc<GraphicsPipeline> {
                &self.pipeline
            }
        }

        // Reset any states that are fixed in the new pipeline. The pipeline bind command will
        // overwrite these states.
        self.current_state.reset_dynamic_states(
            pipeline
                .dynamic_states()
                .filter(|(_, d)| !d) // not dynamic
                .map(|(s, _)| s),
        );
        self.append_command(Cmd { pipeline }, &[]).unwrap();
        self.current_state.pipeline_graphics = self.commands.last().cloned();
    }

    /// Starts the process of binding vertex buffers. Returns an intermediate struct which can be
    /// used to add the buffers.
    #[inline]
    pub fn bind_vertex_buffers(&mut self) -> SyncCommandBufferBuilderBindVertexBuffer {
        SyncCommandBufferBuilderBindVertexBuffer {
            builder: self,
            inner: UnsafeCommandBufferBuilderBindVertexBuffer::new(),
            buffers: SmallVec::new(),
        }
    }

    /// Calls `vkCmdCopyImage` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    #[inline]
    pub unsafe fn copy_image<S, D, R>(
        &mut self,
        source: S,
        source_layout: ImageLayout,
        destination: D,
        destination_layout: ImageLayout,
        regions: R,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        S: ImageAccess + 'static,
        D: ImageAccess + 'static,
        R: IntoIterator<Item = UnsafeCommandBufferBuilderImageCopy> + Send + Sync + 'static,
    {
        struct Cmd<S, D, R> {
            source: S,
            source_layout: ImageLayout,
            destination: D,
            destination_layout: ImageLayout,
            regions: Mutex<Option<R>>,
        }

        impl<S, D, R> Command for Cmd<S, D, R>
        where
            S: ImageAccess + 'static,
            D: ImageAccess + 'static,
            R: IntoIterator<Item = UnsafeCommandBufferBuilderImageCopy> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdCopyImage"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.copy_image(
                    &self.source,
                    self.source_layout,
                    &self.destination,
                    self.destination_layout,
                    self.regions.lock().unwrap().take().unwrap(),
                );
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                if num == 0 {
                    &self.source
                } else if num == 1 {
                    &self.destination
                } else {
                    panic!()
                }
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                if num == 0 {
                    "source".into()
                } else if num == 1 {
                    "destination".into()
                } else {
                    panic!()
                }
            }
        }

        self.append_command(
            Cmd {
                source,
                source_layout,
                destination,
                destination_layout,
                regions: Mutex::new(Some(regions)),
            },
            &[
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_read: true,
                                ..AccessFlags::none()
                            },
                            exclusive: false,
                        },
                        source_layout,
                        source_layout,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_write: true,
                                ..AccessFlags::none()
                            },
                            exclusive: true,
                        },
                        destination_layout,
                        destination_layout,
                        ImageUninitializedSafe::Safe,
                    )),
                ),
            ],
        )?;

        Ok(())
    }

    /// Calls `vkCmdBlitImage` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    #[inline]
    pub unsafe fn blit_image<S, D, R>(
        &mut self,
        source: S,
        source_layout: ImageLayout,
        destination: D,
        destination_layout: ImageLayout,
        regions: R,
        filter: Filter,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        S: ImageAccess + 'static,
        D: ImageAccess + 'static,
        R: IntoIterator<Item = UnsafeCommandBufferBuilderImageBlit> + Send + Sync + 'static,
    {
        struct Cmd<S, D, R> {
            source: S,
            source_layout: ImageLayout,
            destination: D,
            destination_layout: ImageLayout,
            regions: Mutex<Option<R>>,
            filter: Filter,
        }

        impl<S, D, R> Command for Cmd<S, D, R>
        where
            S: ImageAccess + 'static,
            D: ImageAccess + 'static,
            R: IntoIterator<Item = UnsafeCommandBufferBuilderImageBlit> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdBlitImage"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.blit_image(
                    &self.source,
                    self.source_layout,
                    &self.destination,
                    self.destination_layout,
                    self.regions.lock().unwrap().take().unwrap(),
                    self.filter,
                );
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                if num == 0 {
                    &self.source
                } else if num == 1 {
                    &self.destination
                } else {
                    panic!()
                }
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                if num == 0 {
                    "source".into()
                } else if num == 1 {
                    "destination".into()
                } else {
                    panic!()
                }
            }
        }

        self.append_command(
            Cmd {
                source,
                source_layout,
                destination,
                destination_layout,
                regions: Mutex::new(Some(regions)),
                filter,
            },
            &[
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_read: true,
                                ..AccessFlags::none()
                            },
                            exclusive: false,
                        },
                        source_layout,
                        source_layout,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_write: true,
                                ..AccessFlags::none()
                            },
                            exclusive: true,
                        },
                        destination_layout,
                        destination_layout,
                        ImageUninitializedSafe::Safe,
                    )),
                ),
            ],
        )?;

        Ok(())
    }

    /// Calls `vkCmdClearColorImage` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    pub unsafe fn clear_color_image<I, R>(
        &mut self,
        image: I,
        layout: ImageLayout,
        color: ClearValue,
        regions: R,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        I: ImageAccess + 'static,
        R: IntoIterator<Item = UnsafeCommandBufferBuilderColorImageClear> + Send + Sync + 'static,
    {
        struct Cmd<I, R> {
            image: I,
            layout: ImageLayout,
            color: ClearValue,
            regions: Mutex<Option<R>>,
        }

        impl<I, R> Command for Cmd<I, R>
        where
            I: ImageAccess + 'static,
            R: IntoIterator<Item = UnsafeCommandBufferBuilderColorImageClear>
                + Send
                + Sync
                + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdClearColorImage"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.clear_color_image(
                    &self.image,
                    self.layout,
                    self.color,
                    self.regions.lock().unwrap().take().unwrap(),
                );
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                assert_eq!(num, 0);
                &self.image
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "target".into()
            }
        }

        self.append_command(
            Cmd {
                image,
                layout,
                color,
                regions: Mutex::new(Some(regions)),
            },
            &[(
                KeyTy::Image,
                Some((
                    PipelineMemoryAccess {
                        stages: PipelineStages {
                            transfer: true,
                            ..PipelineStages::none()
                        },
                        access: AccessFlags {
                            transfer_write: true,
                            ..AccessFlags::none()
                        },
                        exclusive: true,
                    },
                    layout,
                    layout,
                    ImageUninitializedSafe::Safe,
                )),
            )],
        )?;

        Ok(())
    }

    /// Calls `vkCmdClearDepthStencilImage` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    pub unsafe fn clear_depth_stencil_image<I, R>(
        &mut self,
        image: I,
        layout: ImageLayout,
        clear_value: ClearValue,
        regions: R,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        I: ImageAccess + 'static,
        R: IntoIterator<Item = UnsafeCommandBufferBuilderDepthStencilImageClear>
            + Send
            + Sync
            + 'static,
    {
        struct Cmd<I, R> {
            image: I,
            layout: ImageLayout,
            clear_value: ClearValue,
            regions: Mutex<Option<R>>,
        }

        impl<I, R> Command for Cmd<I, R>
        where
            I: ImageAccess + 'static,
            R: IntoIterator<Item = UnsafeCommandBufferBuilderDepthStencilImageClear>
                + Send
                + Sync
                + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdClearColorImage"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.clear_depth_stencil_image(
                    &self.image,
                    self.layout,
                    self.clear_value,
                    self.regions.lock().unwrap().take().unwrap(),
                );
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                assert_eq!(num, 0);
                &self.image
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "target".into()
            }
        }

        self.append_command(
            Cmd {
                image,
                layout,
                clear_value,
                regions: Mutex::new(Some(regions)),
            },
            &[(
                KeyTy::Image,
                Some((
                    PipelineMemoryAccess {
                        stages: PipelineStages {
                            transfer: true,
                            ..PipelineStages::none()
                        },
                        access: AccessFlags {
                            transfer_write: true,
                            ..AccessFlags::none()
                        },
                        exclusive: true,
                    },
                    layout,
                    layout,
                    ImageUninitializedSafe::Safe,
                )),
            )],
        )?;

        Ok(())
    }

    /// Calls `vkCmdCopyBuffer` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    #[inline]
    pub unsafe fn copy_buffer<S, D, R>(
        &mut self,
        source: S,
        destination: D,
        regions: R,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        S: BufferAccess + 'static,
        D: BufferAccess + 'static,
        R: IntoIterator<Item = (DeviceSize, DeviceSize, DeviceSize)> + Send + Sync + 'static,
    {
        struct Cmd<S, D, R> {
            source: S,
            destination: D,
            regions: Mutex<Option<R>>,
        }

        impl<S, D, R> Command for Cmd<S, D, R>
        where
            S: BufferAccess + 'static,
            D: BufferAccess + 'static,
            R: IntoIterator<Item = (DeviceSize, DeviceSize, DeviceSize)> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdCopyBuffer"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.copy_buffer(
                    &self.source,
                    &self.destination,
                    self.regions.lock().unwrap().take().unwrap(),
                );
            }

            fn buffer(&self, num: usize) -> &dyn BufferAccess {
                match num {
                    0 => &self.source,
                    1 => &self.destination,
                    _ => panic!(),
                }
            }

            fn buffer_name(&self, num: usize) -> Cow<'static, str> {
                match num {
                    0 => "source".into(),
                    1 => "destination".into(),
                    _ => panic!(),
                }
            }
        }

        self.append_command(
            Cmd {
                source,
                destination,
                regions: Mutex::new(Some(regions)),
            },
            &[
                (
                    KeyTy::Buffer,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_read: true,
                                ..AccessFlags::none()
                            },
                            exclusive: false,
                        },
                        ImageLayout::Undefined,
                        ImageLayout::Undefined,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
                (
                    KeyTy::Buffer,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_write: true,
                                ..AccessFlags::none()
                            },
                            exclusive: true,
                        },
                        ImageLayout::Undefined,
                        ImageLayout::Undefined,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
            ],
        )?;

        Ok(())
    }

    /// Calls `vkCmdCopyBufferToImage` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    #[inline]
    pub unsafe fn copy_buffer_to_image<S, D, R>(
        &mut self,
        source: S,
        destination: D,
        destination_layout: ImageLayout,
        regions: R,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        S: BufferAccess + 'static,
        D: ImageAccess + 'static,
        R: IntoIterator<Item = UnsafeCommandBufferBuilderBufferImageCopy> + Send + Sync + 'static,
    {
        struct Cmd<S, D, R> {
            source: S,
            destination: D,
            destination_layout: ImageLayout,
            regions: Mutex<Option<R>>,
        }

        impl<S, D, R> Command for Cmd<S, D, R>
        where
            S: BufferAccess + 'static,
            D: ImageAccess + 'static,
            R: IntoIterator<Item = UnsafeCommandBufferBuilderBufferImageCopy> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdCopyBufferToImage"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.copy_buffer_to_image(
                    &self.source,
                    &self.destination,
                    self.destination_layout,
                    self.regions.lock().unwrap().take().unwrap(),
                );
            }

            fn buffer(&self, num: usize) -> &dyn BufferAccess {
                assert_eq!(num, 0);
                &self.source
            }

            fn buffer_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "source".into()
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                assert_eq!(num, 0);
                &self.destination
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "destination".into()
            }
        }

        self.append_command(
            Cmd {
                source,
                destination,
                destination_layout,
                regions: Mutex::new(Some(regions)),
            },
            &[
                (
                    KeyTy::Buffer,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_read: true,
                                ..AccessFlags::none()
                            },
                            exclusive: false,
                        },
                        ImageLayout::Undefined,
                        ImageLayout::Undefined,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_write: true,
                                ..AccessFlags::none()
                            },
                            exclusive: true,
                        },
                        destination_layout,
                        destination_layout,
                        ImageUninitializedSafe::Safe,
                    )),
                ),
            ],
        )?;

        Ok(())
    }

    /// Calls `vkCmdCopyImageToBuffer` on the builder.
    ///
    /// Does nothing if the list of regions is empty, as it would be a no-op and isn't a valid
    /// usage of the command anyway.
    #[inline]
    pub unsafe fn copy_image_to_buffer<S, D, R>(
        &mut self,
        source: S,
        source_layout: ImageLayout,
        destination: D,
        regions: R,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        S: ImageAccess + 'static,
        D: BufferAccess + 'static,
        R: IntoIterator<Item = UnsafeCommandBufferBuilderBufferImageCopy> + Send + Sync + 'static,
    {
        struct Cmd<S, D, R> {
            source: S,
            source_layout: ImageLayout,
            destination: D,
            regions: Mutex<Option<R>>,
        }

        impl<S, D, R> Command for Cmd<S, D, R>
        where
            S: ImageAccess + 'static,
            D: BufferAccess + 'static,
            R: IntoIterator<Item = UnsafeCommandBufferBuilderBufferImageCopy> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdCopyImageToBuffer"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.copy_image_to_buffer(
                    &self.source,
                    self.source_layout,
                    &self.destination,
                    self.regions.lock().unwrap().take().unwrap(),
                );
            }

            fn buffer(&self, num: usize) -> &dyn BufferAccess {
                assert_eq!(num, 0);
                &self.destination
            }

            fn buffer_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "destination".into()
            }

            fn image(&self, num: usize) -> &dyn ImageAccess {
                assert_eq!(num, 0);
                &self.source
            }

            fn image_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "source".into()
            }
        }

        self.append_command(
            Cmd {
                source,
                destination,
                source_layout,
                regions: Mutex::new(Some(regions)),
            },
            &[
                (
                    KeyTy::Image,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_read: true,
                                ..AccessFlags::none()
                            },
                            exclusive: false,
                        },
                        source_layout,
                        source_layout,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
                (
                    KeyTy::Buffer,
                    Some((
                        PipelineMemoryAccess {
                            stages: PipelineStages {
                                transfer: true,
                                ..PipelineStages::none()
                            },
                            access: AccessFlags {
                                transfer_write: true,
                                ..AccessFlags::none()
                            },
                            exclusive: true,
                        },
                        ImageLayout::Undefined,
                        ImageLayout::Undefined,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ),
            ],
        )?;

        Ok(())
    }

    /// Calls `vkCmdCopyQueryPoolResults` on the builder.
    ///
    /// # Safety
    /// `stride` must be at least the number of bytes that will be written by each query.
    pub unsafe fn copy_query_pool_results<D, T>(
        &mut self,
        query_pool: Arc<QueryPool>,
        queries: Range<u32>,
        destination: D,
        stride: DeviceSize,
        flags: QueryResultFlags,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        D: TypedBufferAccess<Content = [T]> + 'static,
        T: QueryResultElement,
    {
        struct Cmd<D> {
            query_pool: Arc<QueryPool>,
            queries: Range<u32>,
            destination: D,
            stride: DeviceSize,
            flags: QueryResultFlags,
        }

        impl<D, T> Command for Cmd<D>
        where
            D: TypedBufferAccess<Content = [T]> + 'static,
            T: QueryResultElement,
        {
            fn name(&self) -> &'static str {
                "vkCmdCopyQueryPoolResults"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.copy_query_pool_results(
                    self.query_pool.queries_range(self.queries.clone()).unwrap(),
                    &self.destination,
                    self.stride,
                    self.flags,
                );
            }

            fn buffer(&self, num: usize) -> &dyn BufferAccess {
                assert_eq!(num, 0);
                &self.destination
            }

            fn buffer_name(&self, num: usize) -> Cow<'static, str> {
                assert_eq!(num, 0);
                "destination".into()
            }
        }

        self.append_command(
            Cmd {
                query_pool,
                queries,
                destination,
                stride,
                flags,
            },
            &[(
                KeyTy::Buffer,
                Some((
                    PipelineMemoryAccess {
                        stages: PipelineStages {
                            transfer: true,
                            ..PipelineStages::none()
                        },
                        access: AccessFlags {
                            transfer_write: true,
                            ..AccessFlags::none()
                        },
                        exclusive: true,
                    },
                    ImageLayout::Undefined,
                    ImageLayout::Undefined,
                    ImageUninitializedSafe::Unsafe,
                )),
            )],
        )?;

        Ok(())
    }

    /// Calls `vkCmdBeginDebugUtilsLabelEXT` on the builder.
    ///
    /// # Safety
    /// The command pool that this command buffer was allocated from must support graphics or
    /// compute operations
    #[inline]
    pub unsafe fn debug_marker_begin(&mut self, name: &'static CStr, color: [f32; 4]) {
        struct Cmd {
            name: &'static CStr,
            color: [f32; 4],
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdBeginDebugUtilsLabelEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.debug_marker_begin(self.name, self.color);
            }
        }

        self.append_command(Cmd { name, color }, &[]).unwrap();
    }

    /// Calls `vkCmdEndDebugUtilsLabelEXT` on the builder.
    ///
    /// # Safety
    /// - The command pool that this command buffer was allocated from must support graphics or
    /// compute operations
    /// - There must be an outstanding `debug_marker_begin` command prior to the
    /// `debug_marker_end` on the queue.
    #[inline]
    pub unsafe fn debug_marker_end(&mut self) {
        struct Cmd {}

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdEndDebugUtilsLabelEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.debug_marker_end();
            }
        }

        self.append_command(Cmd {}, &[]).unwrap();
    }

    /// Calls `vkCmdInsertDebugUtilsLabelEXT` on the builder.
    ///
    /// # Safety
    /// The command pool that this command buffer was allocated from must support graphics or
    /// compute operations
    #[inline]
    pub unsafe fn debug_marker_insert(&mut self, name: &'static CStr, color: [f32; 4]) {
        struct Cmd {
            name: &'static CStr,
            color: [f32; 4],
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdInsertDebugUtilsLabelEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.debug_marker_insert(self.name, self.color);
            }
        }

        self.append_command(Cmd { name, color }, &[]).unwrap();
    }

    /// Calls `vkCmdDispatch` on the builder.
    #[inline]
    pub unsafe fn dispatch(&mut self, group_counts: [u32; 3]) {
        struct Cmd {
            group_counts: [u32; 3],
            descriptor_sets: SmallVec<[Arc<dyn Command>; 12]>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdDispatch"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.dispatch(self.group_counts);
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(buf) = set.buffer(num) {
                        return buf.0;
                    }
                    num -= set.num_buffers();
                }
                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(buf) = set.buffer(num) {
                        return format!("Buffer bound to set {} descriptor {}", set_num, buf.1)
                            .into();
                    }
                    num -= set.num_buffers();
                }
                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(img) = set.image(num) {
                        return img.0.image();
                    }
                    num -= set.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(img) = set.image(num) {
                        return format!("Image bound to set {} descriptor {}", set_num, img.1)
                            .into();
                    }
                    num -= set.num_images();
                }
                panic!()
            }
        }

        let pipeline = self
            .current_state
            .pipeline_compute
            .as_ref()
            .unwrap()
            .bound_pipeline_compute();

        let mut resources = Vec::new();
        let descriptor_sets = self.add_descriptor_set_resources(
            &mut resources,
            pipeline.layout(),
            PipelineBindPoint::Compute,
        );

        self.append_command(
            Cmd {
                group_counts,
                descriptor_sets,
            },
            &resources,
        )
        .unwrap();
    }

    /// Calls `vkCmdDispatchIndirect` on the builder.
    #[inline]
    pub unsafe fn dispatch_indirect<B>(
        &mut self,
        indirect_buffer: B,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        B: BufferAccess + 'static,
    {
        struct Cmd<B> {
            descriptor_sets: SmallVec<[Arc<dyn Command>; 12]>,
            indirect_buffer: B,
        }

        impl<B> Command for Cmd<B>
        where
            B: BufferAccess + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdDispatchIndirect"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.dispatch_indirect(&self.indirect_buffer);
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(buf) = set.buffer(num) {
                        return buf.0;
                    }
                    num -= set.num_buffers();
                }
                if num == 0 {
                    return &self.indirect_buffer;
                }
                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(buf) = set.buffer(num) {
                        return format!("Buffer bound to set {} descriptor {}", set_num, buf.1)
                            .into();
                    }
                    num -= set.num_buffers();
                }
                if num == 0 {
                    return "indirect buffer".into();
                }
                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(img) = set.image(num) {
                        return img.0.image();
                    }
                    num -= set.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(img) = set.image(num) {
                        return format!("Image bound to set {} descriptor {}", set_num, img.1)
                            .into();
                    }
                    num -= set.num_images();
                }
                panic!()
            }
        }

        let pipeline = self
            .current_state
            .pipeline_compute
            .as_ref()
            .unwrap()
            .bound_pipeline_compute();

        let mut resources = Vec::new();
        let descriptor_sets = self.add_descriptor_set_resources(
            &mut resources,
            pipeline.layout(),
            PipelineBindPoint::Compute,
        );
        self.add_indirect_buffer_resources(&mut resources);

        self.append_command(
            Cmd {
                descriptor_sets,
                indirect_buffer,
            },
            &resources,
        )?;

        Ok(())
    }

    /// Calls `vkCmdDraw` on the builder.
    #[inline]
    pub unsafe fn draw(
        &mut self,
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    ) {
        struct Cmd {
            descriptor_sets: SmallVec<[Arc<dyn Command>; 12]>,
            vertex_buffers: SmallVec<[(u32, Arc<dyn Command>); 4]>,
            vertex_count: u32,
            instance_count: u32,
            first_vertex: u32,
            first_instance: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdDraw"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.draw(
                    self.vertex_count,
                    self.instance_count,
                    self.first_vertex,
                    self.first_instance,
                );
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(buf) = set.buffer(num) {
                        return buf.0;
                    }
                    num -= set.num_buffers();
                }

                for buffer in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, cmd)| cmd.bound_vertex_buffer(*binding_num))
                {
                    if num == 0 {
                        return buffer;
                    }
                    num -= 1;
                }

                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(buf) = set.buffer(num) {
                        return format!("Buffer bound to set {} descriptor {}", set_num, buf.1)
                            .into();
                    }
                    num -= set.num_buffers();
                }

                for binding_num in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, _)| *binding_num)
                {
                    if num == 0 {
                        return format!("Vertex buffer binding {}", binding_num).into();
                    }
                    num -= 1;
                }

                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(img) = set.image(num) {
                        return img.0.image();
                    }
                    num -= set.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(img) = set.image(num) {
                        return format!("Image bound to set {} descriptor {}", set_num, img.1)
                            .into();
                    }
                    num -= set.num_images();
                }
                panic!()
            }
        }

        let pipeline = self
            .current_state
            .pipeline_graphics
            .as_ref()
            .unwrap()
            .bound_pipeline_graphics();

        let mut resources = Vec::new();
        let descriptor_sets = self.add_descriptor_set_resources(
            &mut resources,
            pipeline.layout(),
            PipelineBindPoint::Graphics,
        );
        let vertex_buffers =
            self.add_vertex_buffer_resources(&mut resources, pipeline.vertex_input());

        self.append_command(
            Cmd {
                descriptor_sets,
                vertex_buffers,
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            },
            &resources,
        )
        .unwrap();
    }

    /// Calls `vkCmdDrawIndexed` on the builder.
    #[inline]
    pub unsafe fn draw_indexed(
        &mut self,
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    ) {
        struct Cmd {
            descriptor_sets: SmallVec<[Arc<dyn Command>; 12]>,
            vertex_buffers: SmallVec<[(u32, Arc<dyn Command>); 4]>,
            index_buffer: Arc<dyn Command>,
            index_count: u32,
            instance_count: u32,
            first_index: u32,
            vertex_offset: i32,
            first_instance: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdDrawIndexed"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.draw_indexed(
                    self.index_count,
                    self.instance_count,
                    self.first_index,
                    self.vertex_offset,
                    self.first_instance,
                );
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(buf) = set.buffer(num) {
                        return buf.0;
                    }
                    num -= set.num_buffers();
                }

                for buffer in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, cmd)| cmd.bound_vertex_buffer(*binding_num))
                {
                    if num == 0 {
                        return buffer;
                    }
                    num -= 1;
                }

                if num == 0 {
                    return self.index_buffer.bound_index_buffer().0;
                }

                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(buf) = set.buffer(num) {
                        return format!("Buffer bound to set {} descriptor {}", set_num, buf.1)
                            .into();
                    }
                    num -= set.num_buffers();
                }

                for binding_num in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, _)| *binding_num)
                {
                    if num == 0 {
                        return format!("Vertex buffer binding {}", binding_num).into();
                    }
                    num -= 1;
                }

                if num == 0 {
                    return "index buffer".into();
                }

                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(img) = set.image(num) {
                        return img.0.image();
                    }
                    num -= set.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(img) = set.image(num) {
                        return format!("Image bound to set {} descriptor {}", set_num, img.1)
                            .into();
                    }
                    num -= set.num_images();
                }
                panic!()
            }
        }

        let pipeline = self
            .current_state
            .pipeline_graphics
            .as_ref()
            .unwrap()
            .bound_pipeline_graphics();

        let mut resources = Vec::new();
        let descriptor_sets = self.add_descriptor_set_resources(
            &mut resources,
            pipeline.layout(),
            PipelineBindPoint::Graphics,
        );
        let vertex_buffers =
            self.add_vertex_buffer_resources(&mut resources, pipeline.vertex_input());
        let index_buffer = self.add_index_buffer_resources(&mut resources);

        self.append_command(
            Cmd {
                descriptor_sets,
                vertex_buffers,
                index_buffer,
                index_count,
                instance_count,
                first_index,
                vertex_offset,
                first_instance,
            },
            &resources,
        )
        .unwrap();
    }

    /// Calls `vkCmdDrawIndirect` on the builder.
    #[inline]
    pub unsafe fn draw_indirect<B>(
        &mut self,
        indirect_buffer: B,
        draw_count: u32,
        stride: u32,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        B: BufferAccess + 'static,
    {
        struct Cmd<B> {
            descriptor_sets: SmallVec<[Arc<dyn Command>; 12]>,
            vertex_buffers: SmallVec<[(u32, Arc<dyn Command>); 4]>,
            indirect_buffer: B,
            draw_count: u32,
            stride: u32,
        }

        impl<B> Command for Cmd<B>
        where
            B: BufferAccess + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdDrawIndirect"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.draw_indirect(&self.indirect_buffer, self.draw_count, self.stride);
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(buf) = set.buffer(num) {
                        return buf.0;
                    }
                    num -= set.num_buffers();
                }

                for buffer in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, cmd)| cmd.bound_vertex_buffer(*binding_num))
                {
                    if num == 0 {
                        return buffer;
                    }
                    num -= 1;
                }

                if num == 0 {
                    return &self.indirect_buffer;
                }

                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(buf) = set.buffer(num) {
                        return format!("Buffer bound to set {} descriptor {}", set_num, buf.1)
                            .into();
                    }
                    num -= set.num_buffers();
                }

                for binding_num in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, _)| *binding_num)
                {
                    if num == 0 {
                        return format!("Vertex buffer binding {}", binding_num).into();
                    }
                    num -= 1;
                }

                if num == 0 {
                    return "indirect buffer".into();
                }

                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(img) = set.image(num) {
                        return img.0.image();
                    }
                    num -= set.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(img) = set.image(num) {
                        return format!("Image bound to set {} descriptor {}", set_num, img.1)
                            .into();
                    }
                    num -= set.num_images();
                }
                panic!()
            }
        }

        let pipeline = self
            .current_state
            .pipeline_graphics
            .as_ref()
            .unwrap()
            .bound_pipeline_graphics();

        let mut resources = Vec::new();
        let descriptor_sets = self.add_descriptor_set_resources(
            &mut resources,
            pipeline.layout(),
            PipelineBindPoint::Graphics,
        );
        let vertex_buffers =
            self.add_vertex_buffer_resources(&mut resources, pipeline.vertex_input());
        self.add_indirect_buffer_resources(&mut resources);

        self.append_command(
            Cmd {
                descriptor_sets,
                vertex_buffers,
                indirect_buffer,
                draw_count,
                stride,
            },
            &resources,
        )?;

        Ok(())
    }

    /// Calls `vkCmdDrawIndexedIndirect` on the builder.
    #[inline]
    pub unsafe fn draw_indexed_indirect<B>(
        &mut self,
        indirect_buffer: B,
        draw_count: u32,
        stride: u32,
    ) -> Result<(), SyncCommandBufferBuilderError>
    where
        B: BufferAccess + 'static,
    {
        struct Cmd<B> {
            descriptor_sets: SmallVec<[Arc<dyn Command>; 12]>,
            vertex_buffers: SmallVec<[(u32, Arc<dyn Command>); 4]>,
            index_buffer: Arc<dyn Command>,
            indirect_buffer: B,
            draw_count: u32,
            stride: u32,
        }

        impl<B> Command for Cmd<B>
        where
            B: BufferAccess + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdDrawIndexedIndirect"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.draw_indexed_indirect(&self.indirect_buffer, self.draw_count, self.stride);
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(buf) = set.buffer(num) {
                        return buf.0;
                    }
                    num -= set.num_buffers();
                }

                for buffer in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, cmd)| cmd.bound_vertex_buffer(*binding_num))
                {
                    if num == 0 {
                        return buffer;
                    }
                    num -= 1;
                }

                if num == 0 {
                    return self.index_buffer.bound_index_buffer().0;
                } else if num == 1 {
                    return &self.indirect_buffer;
                }

                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(buf) = set.buffer(num) {
                        return format!("Buffer bound to set {} descriptor {}", set_num, buf.1)
                            .into();
                    }
                    num -= set.num_buffers();
                }

                for binding_num in self
                    .vertex_buffers
                    .iter()
                    .map(|(binding_num, _)| *binding_num)
                {
                    if num == 0 {
                        return format!("Vertex buffer binding {}", binding_num).into();
                    }
                    num -= 1;
                }

                if num == 0 {
                    return "index buffer".into();
                } else if num == 1 {
                    return "indirect buffer".into();
                }

                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for set in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
                {
                    if let Some(img) = set.image(num) {
                        return img.0.image();
                    }
                    num -= set.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (set_num, set) in self
                    .descriptor_sets
                    .iter()
                    .enumerate()
                    .map(|(set_num, cmd)| (set_num, cmd.bound_descriptor_set(set_num as u32).0))
                {
                    if let Some(img) = set.image(num) {
                        return format!("Image bound to set {} descriptor {}", set_num, img.1)
                            .into();
                    }
                    num -= set.num_images();
                }
                panic!()
            }
        }

        let pipeline = self
            .current_state
            .pipeline_graphics
            .as_ref()
            .unwrap()
            .bound_pipeline_graphics();

        let mut resources = Vec::new();
        let descriptor_sets = self.add_descriptor_set_resources(
            &mut resources,
            pipeline.layout(),
            PipelineBindPoint::Graphics,
        );
        let vertex_buffers =
            self.add_vertex_buffer_resources(&mut resources, pipeline.vertex_input());
        let index_buffer = self.add_index_buffer_resources(&mut resources);
        self.add_indirect_buffer_resources(&mut resources);

        self.append_command(
            Cmd {
                descriptor_sets,
                vertex_buffers,
                index_buffer,
                indirect_buffer,
                draw_count,
                stride,
            },
            &resources,
        )?;

        Ok(())
    }

    /// Calls `vkCmdEndQuery` on the builder.
    #[inline]
    pub unsafe fn end_query(&mut self, query_pool: Arc<QueryPool>, query: u32) {
        struct Cmd {
            query_pool: Arc<QueryPool>,
            query: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdEndQuery"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.end_query(self.query_pool.query(self.query).unwrap());
            }
        }

        self.append_command(Cmd { query_pool, query }, &[]).unwrap();
    }

    /// Calls `vkCmdEndRenderPass` on the builder.
    #[inline]
    pub unsafe fn end_render_pass(&mut self) {
        struct Cmd;

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdEndRenderPass"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.end_render_pass();
            }
        }

        self.append_command(Cmd, &[]).unwrap();
        debug_assert!(self.latest_render_pass_enter.is_some());
        self.latest_render_pass_enter = None;
    }

    /// Starts the process of executing secondary command buffers. Returns an intermediate struct
    /// which can be used to add the command buffers.
    #[inline]
    pub unsafe fn execute_commands(&mut self) -> SyncCommandBufferBuilderExecuteCommands {
        SyncCommandBufferBuilderExecuteCommands {
            builder: self,
            inner: Vec::new(),
        }
    }

    /// Calls `vkCmdFillBuffer` on the builder.
    #[inline]
    pub unsafe fn fill_buffer<B>(&mut self, buffer: B, data: u32)
    where
        B: BufferAccess + 'static,
    {
        struct Cmd<B> {
            buffer: B,
            data: u32,
        }

        impl<B> Command for Cmd<B>
        where
            B: BufferAccess + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdFillBuffer"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.fill_buffer(&self.buffer, self.data);
            }

            fn buffer(&self, num: usize) -> &dyn BufferAccess {
                assert_eq!(num, 0);
                &self.buffer
            }

            fn buffer_name(&self, _: usize) -> Cow<'static, str> {
                "destination".into()
            }
        }

        self.append_command(
            Cmd { buffer, data },
            &[(
                KeyTy::Buffer,
                Some((
                    PipelineMemoryAccess {
                        stages: PipelineStages {
                            transfer: true,
                            ..PipelineStages::none()
                        },
                        access: AccessFlags {
                            transfer_write: true,
                            ..AccessFlags::none()
                        },
                        exclusive: true,
                    },
                    ImageLayout::Undefined,
                    ImageLayout::Undefined,
                    ImageUninitializedSafe::Unsafe,
                )),
            )],
        )
        .unwrap();
    }

    /// Calls `vkCmdNextSubpass` on the builder.
    #[inline]
    pub unsafe fn next_subpass(&mut self, subpass_contents: SubpassContents) {
        struct Cmd {
            subpass_contents: SubpassContents,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdNextSubpass"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.next_subpass(self.subpass_contents);
            }
        }

        self.append_command(Cmd { subpass_contents }, &[]).unwrap();
    }

    /// Calls `vkCmdPushConstants` on the builder.
    #[inline]
    pub unsafe fn push_constants<D>(
        &mut self,
        pipeline_layout: Arc<PipelineLayout>,
        stages: ShaderStages,
        offset: u32,
        size: u32,
        data: &D,
    ) where
        D: ?Sized + Send + Sync + 'static,
    {
        struct Cmd {
            pipeline_layout: Arc<PipelineLayout>,
            stages: ShaderStages,
            offset: u32,
            size: u32,
            data: Box<[u8]>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdPushConstants"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.push_constants::<[u8]>(
                    &self.pipeline_layout,
                    self.stages,
                    self.offset,
                    self.size,
                    &self.data,
                );
            }
        }

        debug_assert!(mem::size_of_val(data) >= size as usize);

        let mut out = Vec::with_capacity(size as usize);
        ptr::copy::<u8>(
            data as *const D as *const u8,
            out.as_mut_ptr(),
            size as usize,
        );
        out.set_len(size as usize);

        self.append_command(
            Cmd {
                pipeline_layout: pipeline_layout.clone(),
                stages,
                offset,
                size,
                data: out.into(),
            },
            &[],
        )
        .unwrap();

        // TODO: Push constant invalidations.
        // The Vulkan spec currently is unclear about this, so Vulkano currently just marks
        // push constants as set, and never unsets them. See:
        // https://github.com/KhronosGroup/Vulkan-Docs/issues/1485
        // https://github.com/KhronosGroup/Vulkan-ValidationLayers/issues/2711
        self.current_state
            .push_constants
            .insert(offset..offset + size);
        self.current_state.push_constants_pipeline_layout = Some(pipeline_layout);
    }

    /// Calls `vkCmdResetEvent` on the builder.
    #[inline]
    pub unsafe fn reset_event(&mut self, event: Arc<Event>, stages: PipelineStages) {
        struct Cmd {
            event: Arc<Event>,
            stages: PipelineStages,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdResetEvent"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.reset_event(&self.event, self.stages);
            }
        }

        self.append_command(Cmd { event, stages }, &[]).unwrap();
    }

    /// Calls `vkCmdResetQueryPool` on the builder.
    #[inline]
    pub unsafe fn reset_query_pool(&mut self, query_pool: Arc<QueryPool>, queries: Range<u32>) {
        struct Cmd {
            query_pool: Arc<QueryPool>,
            queries: Range<u32>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdResetQueryPool"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.reset_query_pool(self.query_pool.queries_range(self.queries.clone()).unwrap());
            }
        }

        self.append_command(
            Cmd {
                query_pool,
                queries,
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdSetBlendConstants` on the builder.
    #[inline]
    pub unsafe fn set_blend_constants(&mut self, constants: [f32; 4]) {
        struct Cmd {
            constants: [f32; 4],
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetBlendConstants"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_blend_constants(self.constants);
            }
        }

        self.append_command(Cmd { constants }, &[]).unwrap();
        self.current_state.blend_constants = Some(constants);
    }

    /// Calls `vkCmdSetColorWriteEnableEXT` on the builder.
    ///
    /// If the list is empty then the command is automatically ignored.
    #[inline]
    pub unsafe fn set_color_write_enable<I>(&mut self, enables: I)
    where
        I: IntoIterator<Item = bool>,
    {
        struct Cmd<I> {
            enables: Mutex<Option<I>>,
        }

        impl<I> Command for Cmd<I>
        where
            I: IntoIterator<Item = bool> + Send + Sync,
        {
            fn name(&self) -> &'static str {
                "vkCmdSetColorWriteEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_color_write_enable(self.enables.lock().unwrap().take().unwrap());
            }
        }

        let enables: SmallVec<[bool; 4]> = enables.into_iter().collect();
        self.current_state.color_write_enable = Some(enables.clone());
        self.append_command(
            Cmd {
                enables: Mutex::new(Some(enables)),
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdSetCullModeEXT` on the builder.
    #[inline]
    pub unsafe fn set_cull_mode(&mut self, cull_mode: CullMode) {
        struct Cmd {
            cull_mode: CullMode,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetCullModeEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_cull_mode(self.cull_mode);
            }
        }

        self.append_command(Cmd { cull_mode }, &[]).unwrap();
        self.current_state.cull_mode = Some(cull_mode);
    }

    /// Calls `vkCmdSetDepthBias` on the builder.
    #[inline]
    pub unsafe fn set_depth_bias(&mut self, constant_factor: f32, clamp: f32, slope_factor: f32) {
        struct Cmd {
            constant_factor: f32,
            clamp: f32,
            slope_factor: f32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthBias"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_bias(self.constant_factor, self.clamp, self.slope_factor);
            }
        }

        self.append_command(
            Cmd {
                constant_factor,
                clamp,
                slope_factor,
            },
            &[],
        )
        .unwrap();
        self.current_state.depth_bias = Some(DepthBias {
            constant_factor,
            clamp,
            slope_factor,
        });
    }

    /// Calls `vkCmdSetDepthBiasEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_depth_bias_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthBiasEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_bias_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.depth_bias_enable = Some(enable);
    }

    /// Calls `vkCmdSetDepthBounds` on the builder.
    #[inline]
    pub unsafe fn set_depth_bounds(&mut self, min: f32, max: f32) {
        struct Cmd {
            min: f32,
            max: f32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthBounds"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_bounds(self.min, self.max);
            }
        }

        self.append_command(Cmd { min, max }, &[]).unwrap();
        self.current_state.depth_bounds = Some((min, max));
    }

    /// Calls `vkCmdSetDepthBoundsTestEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_depth_bounds_test_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthBoundsTestEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_bounds_test_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.depth_bounds_test_enable = Some(enable);
    }

    /// Calls `vkCmdSetDepthCompareOpEXT` on the builder.
    #[inline]
    pub unsafe fn set_depth_compare_op(&mut self, compare_op: CompareOp) {
        struct Cmd {
            compare_op: CompareOp,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthCompareOpEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_compare_op(self.compare_op);
            }
        }

        self.append_command(Cmd { compare_op }, &[]).unwrap();
        self.current_state.depth_compare_op = Some(compare_op);
    }

    /// Calls `vkCmdSetDepthTestEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_depth_test_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthTestEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_test_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.depth_test_enable = Some(enable);
    }

    /// Calls `vkCmdSetDepthWriteEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_depth_write_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetDepthWriteEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_depth_write_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.depth_write_enable = Some(enable);
    }

    /// Calls `vkCmdSetDiscardRectangle` on the builder.
    ///
    /// If the list is empty then the command is automatically ignored.
    #[inline]
    pub unsafe fn set_discard_rectangle<I>(&mut self, first_rectangle: u32, rectangles: I)
    where
        I: IntoIterator<Item = Scissor>,
    {
        struct Cmd {
            first_rectangle: u32,
            rectangles: Mutex<SmallVec<[Scissor; 2]>>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetRectangle"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_discard_rectangle(
                    self.first_rectangle,
                    self.rectangles.lock().unwrap().drain(..),
                );
            }
        }

        let rectangles: SmallVec<[Scissor; 2]> = rectangles.into_iter().collect();

        for (num, rectangle) in rectangles.iter().enumerate() {
            let num = num as u32 + first_rectangle;
            self.current_state
                .discard_rectangle
                .insert(num, rectangle.clone());
        }

        self.append_command(
            Cmd {
                first_rectangle,
                rectangles: Mutex::new(rectangles),
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdSetEvent` on the builder.
    #[inline]
    pub unsafe fn set_event(&mut self, event: Arc<Event>, stages: PipelineStages) {
        struct Cmd {
            event: Arc<Event>,
            stages: PipelineStages,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetEvent"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_event(&self.event, self.stages);
            }
        }

        self.append_command(Cmd { event, stages }, &[]).unwrap();
    }

    /// Calls `vkCmdSetFrontFaceEXT` on the builder.
    #[inline]
    pub unsafe fn set_front_face(&mut self, face: FrontFace) {
        struct Cmd {
            face: FrontFace,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetFrontFaceEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_front_face(self.face);
            }
        }

        self.append_command(Cmd { face }, &[]).unwrap();
        self.current_state.front_face = Some(face);
    }

    /// Calls `vkCmdSetLineStippleEXT` on the builder.
    #[inline]
    pub unsafe fn set_line_stipple(&mut self, factor: u32, pattern: u16) {
        struct Cmd {
            factor: u32,
            pattern: u16,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetLineStippleEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_line_stipple(self.factor, self.pattern);
            }
        }

        self.append_command(Cmd { factor, pattern }, &[]).unwrap();
        self.current_state.line_stipple = Some(LineStipple { factor, pattern });
    }

    /// Calls `vkCmdSetLineWidth` on the builder.
    #[inline]
    pub unsafe fn set_line_width(&mut self, line_width: f32) {
        struct Cmd {
            line_width: f32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetLineWidth"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_line_width(self.line_width);
            }
        }

        self.append_command(Cmd { line_width }, &[]).unwrap();
        self.current_state.line_width = Some(line_width);
    }

    /// Calls `vkCmdSetLogicOpEXT` on the builder.
    #[inline]
    pub unsafe fn set_logic_op(&mut self, logic_op: LogicOp) {
        struct Cmd {
            logic_op: LogicOp,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetLogicOpEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_logic_op(self.logic_op);
            }
        }

        self.append_command(Cmd { logic_op }, &[]).unwrap();
        self.current_state.logic_op = Some(logic_op);
    }

    /// Calls `vkCmdSetPatchControlPointsEXT` on the builder.
    #[inline]
    pub unsafe fn set_patch_control_points(&mut self, num: u32) {
        struct Cmd {
            num: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetPatchControlPointsEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_patch_control_points(self.num);
            }
        }

        self.append_command(Cmd { num }, &[]).unwrap();
        self.current_state.patch_control_points = Some(num);
    }

    /// Calls `vkCmdSetPrimitiveRestartEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_primitive_restart_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetPrimitiveRestartEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_primitive_restart_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.primitive_restart_enable = Some(enable);
    }

    /// Calls `vkCmdSetPrimitiveTopologyEXT` on the builder.
    #[inline]
    pub unsafe fn set_primitive_topology(&mut self, topology: PrimitiveTopology) {
        struct Cmd {
            topology: PrimitiveTopology,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetPrimitiveTopologyEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_primitive_topology(self.topology);
            }
        }

        self.append_command(Cmd { topology }, &[]).unwrap();
        self.current_state.primitive_topology = Some(topology);
    }

    /// Calls `vkCmdSetRasterizerDiscardEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_rasterizer_discard_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetRasterizerDiscardEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_rasterizer_discard_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.rasterizer_discard_enable = Some(enable);
    }

    /// Calls `vkCmdSetStencilCompareMask` on the builder.
    #[inline]
    pub unsafe fn set_stencil_compare_mask(&mut self, faces: StencilFaces, compare_mask: u32) {
        struct Cmd {
            faces: StencilFaces,
            compare_mask: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetStencilCompareMask"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_stencil_compare_mask(self.faces, self.compare_mask);
            }
        }

        self.append_command(
            Cmd {
                faces,
                compare_mask,
            },
            &[],
        )
        .unwrap();

        let faces = ash::vk::StencilFaceFlags::from(faces);

        if faces.intersects(ash::vk::StencilFaceFlags::FRONT) {
            self.current_state.stencil_compare_mask.front = Some(compare_mask);
        }

        if faces.intersects(ash::vk::StencilFaceFlags::BACK) {
            self.current_state.stencil_compare_mask.back = Some(compare_mask);
        }
    }

    /// Calls `vkCmdSetStencilOpEXT` on the builder.
    #[inline]
    pub unsafe fn set_stencil_op(
        &mut self,
        faces: StencilFaces,
        fail_op: StencilOp,
        pass_op: StencilOp,
        depth_fail_op: StencilOp,
        compare_op: CompareOp,
    ) {
        struct Cmd {
            faces: StencilFaces,
            fail_op: StencilOp,
            pass_op: StencilOp,
            depth_fail_op: StencilOp,
            compare_op: CompareOp,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetStencilOpEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_stencil_op(
                    self.faces,
                    self.fail_op,
                    self.pass_op,
                    self.depth_fail_op,
                    self.compare_op,
                );
            }
        }

        self.append_command(
            Cmd {
                faces,
                fail_op,
                pass_op,
                depth_fail_op,
                compare_op,
            },
            &[],
        )
        .unwrap();

        let faces = ash::vk::StencilFaceFlags::from(faces);

        if faces.intersects(ash::vk::StencilFaceFlags::FRONT) {
            self.current_state.stencil_op.front = Some(StencilOps {
                fail_op,
                pass_op,
                depth_fail_op,
                compare_op,
            });
        }

        if faces.intersects(ash::vk::StencilFaceFlags::BACK) {
            self.current_state.stencil_op.back = Some(StencilOps {
                fail_op,
                pass_op,
                depth_fail_op,
                compare_op,
            });
        }
    }

    /// Calls `vkCmdSetStencilReference` on the builder.
    #[inline]
    pub unsafe fn set_stencil_reference(&mut self, faces: StencilFaces, reference: u32) {
        struct Cmd {
            faces: StencilFaces,
            reference: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetStencilReference"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_stencil_reference(self.faces, self.reference);
            }
        }

        self.append_command(Cmd { faces, reference }, &[]).unwrap();

        let faces = ash::vk::StencilFaceFlags::from(faces);

        if faces.intersects(ash::vk::StencilFaceFlags::FRONT) {
            self.current_state.stencil_reference.front = Some(reference);
        }

        if faces.intersects(ash::vk::StencilFaceFlags::BACK) {
            self.current_state.stencil_reference.back = Some(reference);
        }
    }

    /// Calls `vkCmdSetStencilTestEnableEXT` on the builder.
    #[inline]
    pub unsafe fn set_stencil_test_enable(&mut self, enable: bool) {
        struct Cmd {
            enable: bool,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetStencilTestEnableEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_stencil_test_enable(self.enable);
            }
        }

        self.append_command(Cmd { enable }, &[]).unwrap();
        self.current_state.stencil_test_enable = Some(enable);
    }

    /// Calls `vkCmdSetStencilWriteMask` on the builder.
    #[inline]
    pub unsafe fn set_stencil_write_mask(&mut self, faces: StencilFaces, write_mask: u32) {
        struct Cmd {
            faces: StencilFaces,
            write_mask: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetStencilWriteMask"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_stencil_write_mask(self.faces, self.write_mask);
            }
        }

        self.append_command(Cmd { faces, write_mask }, &[]).unwrap();

        let faces = ash::vk::StencilFaceFlags::from(faces);

        if faces.intersects(ash::vk::StencilFaceFlags::FRONT) {
            self.current_state.stencil_write_mask.front = Some(write_mask);
        }

        if faces.intersects(ash::vk::StencilFaceFlags::BACK) {
            self.current_state.stencil_write_mask.back = Some(write_mask);
        }
    }

    /// Calls `vkCmdSetScissor` on the builder.
    ///
    /// If the list is empty then the command is automatically ignored.
    #[inline]
    pub unsafe fn set_scissor<I>(&mut self, first_scissor: u32, scissors: I)
    where
        I: IntoIterator<Item = Scissor>,
    {
        struct Cmd {
            first_scissor: u32,
            scissors: Mutex<SmallVec<[Scissor; 2]>>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetScissor"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_scissor(self.first_scissor, self.scissors.lock().unwrap().drain(..));
            }
        }

        let scissors: SmallVec<[Scissor; 2]> = scissors.into_iter().collect();

        for (num, scissor) in scissors.iter().enumerate() {
            let num = num as u32 + first_scissor;
            self.current_state.scissor.insert(num, scissor.clone());
        }

        self.append_command(
            Cmd {
                first_scissor,
                scissors: Mutex::new(scissors),
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdSetScissorWithCountEXT` on the builder.
    ///
    /// If the list is empty then the command is automatically ignored.
    #[inline]
    pub unsafe fn set_scissor_with_count<I>(&mut self, scissors: I)
    where
        I: IntoIterator<Item = Scissor>,
    {
        struct Cmd {
            scissors: Mutex<SmallVec<[Scissor; 2]>>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetScissorWithCountEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_scissor_with_count(self.scissors.lock().unwrap().drain(..));
            }
        }

        let scissors: SmallVec<[Scissor; 2]> = scissors.into_iter().collect();
        self.current_state.scissor_with_count = Some(scissors.clone());
        self.append_command(
            Cmd {
                scissors: Mutex::new(scissors),
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdSetViewport` on the builder.
    ///
    /// If the list is empty then the command is automatically ignored.
    #[inline]
    pub unsafe fn set_viewport<I>(&mut self, first_viewport: u32, viewports: I)
    where
        I: IntoIterator<Item = Viewport>,
    {
        struct Cmd {
            first_viewport: u32,
            viewports: Mutex<SmallVec<[Viewport; 2]>>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetViewport"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_viewport(
                    self.first_viewport,
                    self.viewports.lock().unwrap().drain(..),
                );
            }
        }

        let viewports: SmallVec<[Viewport; 2]> = viewports.into_iter().collect();

        for (num, viewport) in viewports.iter().enumerate() {
            let num = num as u32 + first_viewport;
            self.current_state.viewport.insert(num, viewport.clone());
        }

        self.append_command(
            Cmd {
                first_viewport,
                viewports: Mutex::new(viewports),
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdSetViewportWithCountEXT` on the builder.
    ///
    /// If the list is empty then the command is automatically ignored.
    #[inline]
    pub unsafe fn set_viewport_with_count<I>(&mut self, viewports: I)
    where
        I: IntoIterator<Item = Viewport>,
    {
        struct Cmd {
            viewports: Mutex<SmallVec<[Viewport; 2]>>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdSetViewportWithCountEXT"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.set_viewport_with_count(self.viewports.lock().unwrap().drain(..));
            }
        }

        let viewports: SmallVec<[Viewport; 2]> = viewports.into_iter().collect();
        self.current_state.viewport_with_count = Some(viewports.clone());
        self.append_command(
            Cmd {
                viewports: Mutex::new(viewports),
            },
            &[],
        )
        .unwrap();
    }

    /// Calls `vkCmdUpdateBuffer` on the builder.
    #[inline]
    pub unsafe fn update_buffer<B, D, Dd>(&mut self, buffer: B, data: Dd)
    where
        B: BufferAccess + 'static,
        D: ?Sized,
        Dd: SafeDeref<Target = D> + Send + Sync + 'static,
    {
        struct Cmd<B, Dd> {
            buffer: B,
            data: Dd,
        }

        impl<B, D, Dd> Command for Cmd<B, Dd>
        where
            B: BufferAccess + 'static,
            D: ?Sized,
            Dd: SafeDeref<Target = D> + Send + Sync + 'static,
        {
            fn name(&self) -> &'static str {
                "vkCmdUpdateBuffer"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.update_buffer(&self.buffer, self.data.deref());
            }

            fn buffer(&self, num: usize) -> &dyn BufferAccess {
                assert_eq!(num, 0);
                &self.buffer
            }

            fn buffer_name(&self, _: usize) -> Cow<'static, str> {
                "destination".into()
            }
        }

        self.append_command(
            Cmd { buffer, data },
            &[(
                KeyTy::Buffer,
                Some((
                    PipelineMemoryAccess {
                        stages: PipelineStages {
                            transfer: true,
                            ..PipelineStages::none()
                        },
                        access: AccessFlags {
                            transfer_write: true,
                            ..AccessFlags::none()
                        },
                        exclusive: true,
                    },
                    ImageLayout::Undefined,
                    ImageLayout::Undefined,
                    ImageUninitializedSafe::Unsafe,
                )),
            )],
        )
        .unwrap();
    }

    /// Calls `vkCmdWriteTimestamp` on the builder.
    #[inline]
    pub unsafe fn write_timestamp(
        &mut self,
        query_pool: Arc<QueryPool>,
        query: u32,
        stage: PipelineStage,
    ) {
        struct Cmd {
            query_pool: Arc<QueryPool>,
            query: u32,
            stage: PipelineStage,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdWriteTimestamp"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.write_timestamp(self.query_pool.query(self.query).unwrap(), self.stage);
            }
        }

        self.append_command(
            Cmd {
                query_pool,
                query,
                stage,
            },
            &[],
        )
        .unwrap();
    }

    fn add_descriptor_set_resources(
        &self,
        resources: &mut Vec<(
            KeyTy,
            Option<(
                PipelineMemoryAccess,
                ImageLayout,
                ImageLayout,
                ImageUninitializedSafe,
            )>,
        )>,
        pipeline_layout: &PipelineLayout,
        pipeline_bind_point: PipelineBindPoint,
    ) -> SmallVec<[Arc<dyn Command>; 12]> {
        let descriptor_sets: SmallVec<[Arc<dyn Command>; 12]> = (0..pipeline_layout
            .descriptor_set_layouts()
            .len() as u32)
            .map(|set_num| {
                self.current_state.descriptor_sets[&pipeline_bind_point].descriptor_sets[&set_num]
                    .clone()
            })
            .collect();

        for ds in descriptor_sets
            .iter()
            .enumerate()
            .map(|(set_num, cmd)| cmd.bound_descriptor_set(set_num as u32).0)
        {
            for buf_num in 0..ds.num_buffers() {
                let desc = ds
                    .layout()
                    .descriptor(ds.buffer(buf_num).unwrap().1)
                    .unwrap();
                let exclusive = desc.mutable;
                let (stages, access) = desc.pipeline_stages_and_access();
                resources.push((
                    KeyTy::Buffer,
                    Some((
                        PipelineMemoryAccess {
                            stages,
                            access,
                            exclusive,
                        },
                        ImageLayout::Undefined,
                        ImageLayout::Undefined,
                        ImageUninitializedSafe::Unsafe,
                    )),
                ));
            }
            for img_num in 0..ds.num_images() {
                let (image_view, desc_num) = ds.image(img_num).unwrap();
                let desc = ds.layout().descriptor(desc_num).unwrap();
                let exclusive = desc.mutable;
                let (stages, access) = desc.pipeline_stages_and_access();
                let mut ignore_me_hack = false;
                let layouts = image_view
                    .image()
                    .descriptor_layouts()
                    .expect("descriptor_layouts must return Some when used in an image view");
                let layout = match desc.ty {
                    DescriptorDescTy::CombinedImageSampler { .. } => layouts.combined_image_sampler,
                    DescriptorDescTy::SampledImage { .. } => layouts.sampled_image,
                    DescriptorDescTy::StorageImage { .. } => layouts.storage_image,
                    DescriptorDescTy::InputAttachment { .. } => {
                        // FIXME: This is tricky. Since we read from the input attachment
                        // and this input attachment is being written in an earlier pass,
                        // vulkano will think that it needs to put a pipeline barrier and will
                        // return a `Conflict` error. For now as a work-around we simply ignore
                        // input attachments.
                        ignore_me_hack = true;
                        layouts.input_attachment
                    }
                    _ => panic!("Tried to bind an image to a non-image descriptor"),
                };
                resources.push((
                    KeyTy::Image,
                    if ignore_me_hack {
                        None
                    } else {
                        Some((
                            PipelineMemoryAccess {
                                stages,
                                access,
                                exclusive,
                            },
                            layout,
                            layout,
                            ImageUninitializedSafe::Unsafe,
                        ))
                    },
                ));
            }
        }

        descriptor_sets
    }

    fn add_vertex_buffer_resources(
        &self,
        resources: &mut Vec<(
            KeyTy,
            Option<(
                PipelineMemoryAccess,
                ImageLayout,
                ImageLayout,
                ImageUninitializedSafe,
            )>,
        )>,
        vertex_input: &VertexInput,
    ) -> SmallVec<[(u32, Arc<dyn Command>); 4]> {
        let vertex_buffers: SmallVec<[(u32, Arc<dyn Command>); 4]> = vertex_input
            .bindings()
            .map(|(binding_num, _)| {
                (
                    binding_num,
                    self.current_state.vertex_buffers[&binding_num].clone(),
                )
            })
            .collect();

        resources.extend(vertex_buffers.iter().map(|_| {
            (
                KeyTy::Buffer,
                Some((
                    PipelineMemoryAccess {
                        stages: PipelineStages {
                            vertex_input: true,
                            ..PipelineStages::none()
                        },
                        access: AccessFlags {
                            vertex_attribute_read: true,
                            ..AccessFlags::none()
                        },
                        exclusive: false,
                    },
                    ImageLayout::Undefined,
                    ImageLayout::Undefined,
                    ImageUninitializedSafe::Unsafe,
                )),
            )
        }));

        vertex_buffers
    }

    fn add_index_buffer_resources(
        &self,
        resources: &mut Vec<(
            KeyTy,
            Option<(
                PipelineMemoryAccess,
                ImageLayout,
                ImageLayout,
                ImageUninitializedSafe,
            )>,
        )>,
    ) -> Arc<dyn Command> {
        let index_buffer = self.current_state.index_buffer.as_ref().unwrap().clone();

        resources.push((
            KeyTy::Buffer,
            Some((
                PipelineMemoryAccess {
                    stages: PipelineStages {
                        vertex_input: true,
                        ..PipelineStages::none()
                    },
                    access: AccessFlags {
                        index_read: true,
                        ..AccessFlags::none()
                    },
                    exclusive: false,
                },
                ImageLayout::Undefined,
                ImageLayout::Undefined,
                ImageUninitializedSafe::Unsafe,
            )),
        ));

        index_buffer
    }

    fn add_indirect_buffer_resources(
        &self,
        resources: &mut Vec<(
            KeyTy,
            Option<(
                PipelineMemoryAccess,
                ImageLayout,
                ImageLayout,
                ImageUninitializedSafe,
            )>,
        )>,
    ) {
        resources.push((
            KeyTy::Buffer,
            Some((
                PipelineMemoryAccess {
                    stages: PipelineStages {
                        draw_indirect: true,
                        ..PipelineStages::none()
                    }, // TODO: is draw_indirect correct for dispatch too?
                    access: AccessFlags {
                        indirect_command_read: true,
                        ..AccessFlags::none()
                    },
                    exclusive: false,
                },
                ImageLayout::Undefined,
                ImageLayout::Undefined,
                ImageUninitializedSafe::Unsafe,
            )),
        ));
    }
}

pub struct SyncCommandBufferBuilderBindDescriptorSets<'b> {
    builder: &'b mut SyncCommandBufferBuilder,
    descriptor_sets: SmallVec<[DescriptorSetWithOffsets; 12]>,
}

impl<'b> SyncCommandBufferBuilderBindDescriptorSets<'b> {
    /// Adds a descriptor set to the list.
    #[inline]
    pub fn add<S>(&mut self, descriptor_set: S)
    where
        S: Into<DescriptorSetWithOffsets>,
    {
        self.descriptor_sets.push(descriptor_set.into());
    }

    #[inline]
    pub unsafe fn submit(
        self,
        pipeline_bind_point: PipelineBindPoint,
        pipeline_layout: Arc<PipelineLayout>,
        first_set: u32,
    ) {
        if self.descriptor_sets.is_empty() {
            return;
        }

        struct Cmd {
            descriptor_sets: SmallVec<[DescriptorSetWithOffsets; 12]>,
            pipeline_bind_point: PipelineBindPoint,
            pipeline_layout: Arc<PipelineLayout>,
            first_set: u32,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdBindDescriptorSets"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                let descriptor_sets = self.descriptor_sets.iter().map(|x| x.as_ref().0.inner());
                let dynamic_offsets = self
                    .descriptor_sets
                    .iter()
                    .map(|x| x.as_ref().1.iter().copied())
                    .flatten();

                out.bind_descriptor_sets(
                    self.pipeline_bind_point,
                    &self.pipeline_layout,
                    self.first_set,
                    descriptor_sets,
                    dynamic_offsets,
                );
            }

            fn bound_descriptor_set(&self, set_num: u32) -> (&dyn DescriptorSet, &[u32]) {
                let index = set_num.checked_sub(self.first_set).unwrap() as usize;
                self.descriptor_sets[index].as_ref()
            }
        }

        let num_descriptor_sets = self.descriptor_sets.len() as u32;
        self.builder
            .append_command(
                Cmd {
                    descriptor_sets: self.descriptor_sets,
                    pipeline_bind_point,
                    pipeline_layout: pipeline_layout.clone(),
                    first_set,
                },
                &[],
            )
            .unwrap();

        let cmd = self.builder.commands.last().unwrap();
        let state = match self
            .builder
            .current_state
            .descriptor_sets
            .entry(pipeline_bind_point)
        {
            Entry::Vacant(entry) => entry.insert(DescriptorSetState {
                descriptor_sets: Default::default(),
                pipeline_layout,
            }),
            Entry::Occupied(entry) => {
                let state = entry.into_mut();

                let invalidate_from = if state.pipeline_layout.internal_object()
                    == pipeline_layout.internal_object()
                {
                    // If we're still using the exact same layout, then of course it's compatible.
                    None
                } else if state.pipeline_layout.push_constant_ranges()
                    != pipeline_layout.push_constant_ranges()
                {
                    // If the push constant ranges don't match,
                    // all bound descriptor sets are disturbed.
                    Some(0)
                } else {
                    // Find the first descriptor set layout in the current pipeline layout that
                    // isn't compatible with the corresponding set in the new pipeline layout.
                    // If an incompatible set was found, all bound sets from that slot onwards will
                    // be disturbed.
                    let current_layouts = state.pipeline_layout.descriptor_set_layouts();
                    let new_layouts = pipeline_layout.descriptor_set_layouts();
                    let max = (current_layouts.len() as u32).min(first_set + num_descriptor_sets);
                    (0..max).find(|&num| {
                        let num = num as usize;
                        !current_layouts[num].is_compatible_with(&new_layouts[num])
                    })
                };

                if let Some(invalidate_from) = invalidate_from {
                    // Remove disturbed sets and set new pipeline layout.
                    state
                        .descriptor_sets
                        .retain(|&num, _| num < invalidate_from);
                    state.pipeline_layout = pipeline_layout;
                } else if (first_set + num_descriptor_sets) as usize
                    >= state.pipeline_layout.descriptor_set_layouts().len()
                {
                    // New layout is a superset of the old one.
                    state.pipeline_layout = pipeline_layout;
                }

                state
            }
        };

        for i in 0..num_descriptor_sets {
            state.descriptor_sets.insert(first_set + i, cmd.clone());
        }
    }
}

/// Prototype for a `vkCmdBindVertexBuffers`.
pub struct SyncCommandBufferBuilderBindVertexBuffer<'a> {
    builder: &'a mut SyncCommandBufferBuilder,
    inner: UnsafeCommandBufferBuilderBindVertexBuffer,
    buffers: SmallVec<[Box<dyn BufferAccess>; 4]>,
}

impl<'a> SyncCommandBufferBuilderBindVertexBuffer<'a> {
    /// Adds a buffer to the list.
    #[inline]
    pub fn add<B>(&mut self, buffer: B)
    where
        B: BufferAccess + 'static,
    {
        self.inner.add(&buffer);
        self.buffers.push(Box::new(buffer));
    }

    #[inline]
    pub unsafe fn submit(self, first_set: u32) {
        struct Cmd {
            first_set: u32,
            inner: Mutex<Option<UnsafeCommandBufferBuilderBindVertexBuffer>>,
            buffers: SmallVec<[Box<dyn BufferAccess>; 4]>,
        }

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdBindVertexBuffers"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                out.bind_vertex_buffers(self.first_set, self.inner.lock().unwrap().take().unwrap());
            }

            fn bound_vertex_buffer(&self, binding_num: u32) -> &dyn BufferAccess {
                let index = binding_num.checked_sub(self.first_set).unwrap() as usize;
                &self.buffers[index]
            }
        }

        let num_buffers = self.buffers.len() as u32;
        self.builder
            .append_command(
                Cmd {
                    first_set,
                    inner: Mutex::new(Some(self.inner)),
                    buffers: self.buffers,
                },
                &[],
            )
            .unwrap();

        let cmd = self.builder.commands.last().unwrap();
        for i in 0..num_buffers {
            self.builder
                .current_state
                .vertex_buffers
                .insert(first_set + i, cmd.clone());
        }
    }
}

/// Prototype for a `vkCmdExecuteCommands`.
pub struct SyncCommandBufferBuilderExecuteCommands<'a> {
    builder: &'a mut SyncCommandBufferBuilder,
    inner: Vec<Box<dyn SecondaryCommandBuffer>>,
}

impl<'a> SyncCommandBufferBuilderExecuteCommands<'a> {
    /// Adds a command buffer to the list.
    #[inline]
    pub fn add<C>(&mut self, command_buffer: C)
    where
        C: SecondaryCommandBuffer + 'static,
    {
        self.inner.push(Box::new(command_buffer));
    }

    #[inline]
    pub unsafe fn submit(self) -> Result<(), SyncCommandBufferBuilderError> {
        struct DropUnlock(Box<dyn SecondaryCommandBuffer>);
        impl std::ops::Deref for DropUnlock {
            type Target = Box<dyn SecondaryCommandBuffer>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
        unsafe impl SafeDeref for DropUnlock {}

        impl Drop for DropUnlock {
            fn drop(&mut self) {
                unsafe {
                    self.unlock();
                }
            }
        }

        struct Cmd(Vec<DropUnlock>);

        impl Command for Cmd {
            fn name(&self) -> &'static str {
                "vkCmdExecuteCommands"
            }

            unsafe fn send(&self, out: &mut UnsafeCommandBufferBuilder) {
                let mut execute = UnsafeCommandBufferBuilderExecuteCommands::new();
                self.0
                    .iter()
                    .for_each(|cbuf| execute.add_raw(cbuf.inner().internal_object()));
                out.execute_commands(execute);
            }

            fn buffer(&self, mut num: usize) -> &dyn BufferAccess {
                for cbuf in self.0.iter() {
                    if let Some(buf) = cbuf.buffer(num) {
                        return buf.0;
                    }
                    num -= cbuf.num_buffers();
                }
                panic!()
            }

            fn buffer_name(&self, mut num: usize) -> Cow<'static, str> {
                for (cbuf_num, cbuf) in self.0.iter().enumerate() {
                    if let Some(buf) = cbuf.buffer(num) {
                        return format!("Buffer bound to secondary command buffer {}", cbuf_num)
                            .into();
                    }
                    num -= cbuf.num_buffers();
                }
                panic!()
            }

            fn image(&self, mut num: usize) -> &dyn ImageAccess {
                for cbuf in self.0.iter() {
                    if let Some(img) = cbuf.image(num) {
                        return img.0;
                    }
                    num -= cbuf.num_images();
                }
                panic!()
            }

            fn image_name(&self, mut num: usize) -> Cow<'static, str> {
                for (cbuf_num, cbuf) in self.0.iter().enumerate() {
                    if let Some(img) = cbuf.image(num) {
                        return format!("Image bound to secondary command buffer {}", cbuf_num)
                            .into();
                    }
                    num -= cbuf.num_images();
                }
                panic!()
            }
        }

        let resources = {
            let mut resources = Vec::new();
            for cbuf in self.inner.iter() {
                for buf_num in 0..cbuf.num_buffers() {
                    resources.push((
                        KeyTy::Buffer,
                        Some((
                            cbuf.buffer(buf_num).unwrap().1,
                            ImageLayout::Undefined,
                            ImageLayout::Undefined,
                            ImageUninitializedSafe::Unsafe,
                        )),
                    ));
                }
                for img_num in 0..cbuf.num_images() {
                    let (_, memory, start_layout, end_layout, image_uninitialized_safe) =
                        cbuf.image(img_num).unwrap();
                    resources.push((
                        KeyTy::Image,
                        Some((memory, start_layout, end_layout, image_uninitialized_safe)),
                    ));
                }
            }
            resources
        };

        self.builder.append_command(
            Cmd(self
                .inner
                .into_iter()
                .map(|cbuf| {
                    cbuf.lock_record()?;
                    Ok(DropUnlock(cbuf))
                })
                .collect::<Result<Vec<_>, CommandBufferExecError>>()?),
            &resources,
        )?;

        Ok(())
    }
}
