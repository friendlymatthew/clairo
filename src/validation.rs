use std::{num::NonZeroU64, time::Duration};

use anyhow::{Result, ensure};

use crate::{GpuHandle, read_mapped_buffer};

#[derive(Debug)]
pub struct ValidationContext<'a> {
    gpu: GpuHandle<'a>,
    timeout: Duration,
}

impl<'a> ValidationContext<'a> {
    pub(crate) fn new(gpu: GpuHandle<'a>, timeout: Duration) -> Self {
        Self { gpu, timeout }
    }

    pub fn gpu(&self) -> &GpuHandle<'a> {
        &self.gpu
    }

    pub async fn read_buffer(
        &mut self,
        source: &wgpu::Buffer,
        source_offset: wgpu::BufferAddress,
        size: NonZeroU64,
    ) -> Result<Vec<u8>> {
        let size = size.get();

        ensure!(
            source.usage().contains(wgpu::BufferUsages::COPY_SRC),
            "validation source needs COPY_SRC usage"
        );

        ensure!(
            source_offset.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            "source offset must be copy-aligned"
        );

        ensure!(
            size.is_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            "readback size must be copy-aligned"
        );

        ensure!(
            source_offset
                .checked_add(size)
                .is_some_and(|end| end <= source.size()),
            "readback exceeds source buffer"
        );

        let device = self.gpu.device();
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clairo validation readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clairo validation copy"),
        });

        encoder.copy_buffer_to_buffer(source, source_offset, &readback, 0, size);

        let submission = self.gpu.queue().submit([encoder.finish()]);

        read_mapped_buffer(device, &readback, submission, self.timeout)
    }
}
