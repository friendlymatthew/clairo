use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow, ensure};

pub(crate) fn read_mapped_buffer(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    timeout: Duration,
) -> Result<Vec<u8>> {
    ensure!(
        !cfg!(target_arch = "wasm32"),
        "blocking readback requires a native target"
    );

    let slice = buffer.slice(..);
    let (sender, mut receiver) = futures_channel::oneshot::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });

    let result = (|| {
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(timeout),
        })?;
        receiver
            .try_recv()?
            .ok_or_else(|| anyhow!("GPU buffer mapping did not complete"))??;
        let mapped = slice.get_mapped_range()?;

        Ok(mapped.to_vec())
    })();
    buffer.unmap();

    result
}

/// a borrowed handle to the selected device during setup
#[derive(Debug, Copy, Clone)]
pub struct GpuHandle<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
}

impl<'a> GpuHandle<'a> {
    pub(crate) fn new(device: &'a wgpu::Device, queue: &'a wgpu::Queue) -> Self {
        Self { device, queue }
    }

    pub fn device(&self) -> &'a wgpu::Device {
        self.device
    }

    pub fn queue(&self) -> &'a wgpu::Queue {
        self.queue
    }
}

/// capabilities a benchmark needs in addition to timestamp queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuRequirements {
    required_features: wgpu::Features,
    required_limits: wgpu::Limits,
}

impl Default for GpuRequirements {
    fn default() -> Self {
        Self {
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
        }
    }
}

impl GpuRequirements {
    pub fn new(required_features: wgpu::Features, required_limits: wgpu::Limits) -> Self {
        Self {
            required_features,
            required_limits,
        }
    }

    pub fn required_features(&self) -> wgpu::Features {
        self.required_features
    }

    pub fn required_limits(&self) -> &wgpu::Limits {
        &self.required_limits
    }

    #[must_use]
    pub fn with_required_features(mut self, features: wgpu::Features) -> Self {
        self.required_features |= features;

        self
    }

    #[must_use]
    pub fn with_required_limits(mut self, limits: wgpu::Limits) -> Self {
        self.required_limits = limits;

        self
    }
}

pub(crate) struct GpuSession {
    _instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    errors: DeviceErrors,
}

impl GpuSession {
    pub(crate) async fn try_request(
        backends: wgpu::Backends,
        requirements: GpuRequirements,
    ) -> Result<Self> {
        let GpuRequirements {
            required_features,
            required_limits,
        } = requirements.with_required_features(wgpu::Features::TIMESTAMP_QUERY);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let adapters = instance.enumerate_adapters(backends).await;
        let adapter = adapters
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();

                !is_software_adapter(&info)
                    && adapter.features().contains(required_features)
                    && required_limits.check_limits(&adapter.limits())
            })
            // we perform this sort for deterministic selection, (comparisons, etc.)
            .min_by_key(|adapter| adapter_selection_key(&adapter.get_info()))
            .ok_or_else(|| anyhow!("no compatible hardware GPU adapter was found"))?;

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("clairo benchmark device"),
            required_features,
            required_limits,
            memory_hints: wgpu::MemoryHints::Performance,
            ..wgpu::DeviceDescriptor::default()
        };

        let (device, queue) = adapter
            .request_device(&descriptor)
            .await
            .map_err(|error| anyhow!("requesting the GPU device failed: {error}"))?;

        let errors = DeviceErrors::default();
        errors.install(&device);

        let timestamp_period_ns = queue.get_timestamp_period();
        ensure!(
            timestamp_period_ns.is_finite() && timestamp_period_ns > 0.0,
            "the selected adapter reported invalid timestamp period {} ns",
            timestamp_period_ns
        );

        Ok(Self {
            _instance: instance,
            device,
            queue,
            errors,
        })
    }

    pub(crate) fn gpu(&self) -> GpuHandle<'_> {
        GpuHandle::new(&self.device, &self.queue)
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub(crate) fn errors(&self) -> &DeviceErrors {
        &self.errors
    }
}

type AdapterSelectionKey = (u8, u8, u32, u32, String, String, String, String);

fn is_software_adapter(info: &wgpu::AdapterInfo) -> bool {
    if info.device_type == wgpu::DeviceType::Cpu || info.backend == wgpu::Backend::Noop {
        return true;
    }

    let identity = format!("{} {} {}", info.name, info.driver, info.driver_info).to_lowercase();

    ["swiftshader", "llvmpipe", "lavapipe", "software"]
        .iter()
        .any(|marker| identity.contains(marker))
}

fn adapter_selection_key(info: &wgpu::AdapterInfo) -> AdapterSelectionKey {
    (
        device_type_rank(info.device_type),
        backend_rank(info.backend),
        info.vendor,
        info.device,
        info.device_pci_bus_id.clone(),
        info.name.clone(),
        info.driver.clone(),
        info.driver_info.clone(),
    )
}

fn device_type_rank(device_type: wgpu::DeviceType) -> u8 {
    match device_type {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

fn backend_rank(backend: wgpu::Backend) -> u8 {
    match backend {
        wgpu::Backend::Metal => 0,
        wgpu::Backend::Dx12 => 1,
        wgpu::Backend::Vulkan => 2,
        wgpu::Backend::BrowserWebGpu => 3,
        wgpu::Backend::Gl => 4,
        wgpu::Backend::Noop => 5,
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct DeviceErrors {
    messages: Arc<Mutex<Vec<String>>>,
}

impl DeviceErrors {
    fn install(&self, device: &wgpu::Device) {
        let uncaptured = self.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            uncaptured.push(format!("uncaptured GPU error: {error}"));
        }));

        let lost = self.clone();
        device.set_device_lost_callback(move |reason, message| {
            lost.push(format!("GPU device lost ({reason:?}): {message}"));
        });
    }

    fn push(&self, message: String) {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(message);
    }

    pub(crate) fn snapshot(&self) -> Vec<String> {
        self.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_detection_checks_type_backend_and_identity() {
        let cpu = wgpu::AdapterInfo::new(wgpu::DeviceType::Cpu, wgpu::Backend::Vulkan);
        let noop = wgpu::AdapterInfo::new(wgpu::DeviceType::Other, wgpu::Backend::Noop);

        assert!(is_software_adapter(&cpu));
        assert!(is_software_adapter(&noop));

        for (name, driver, driver_info) in [
            ("SwiftShader Device", "", ""),
            ("", "LLVMpipe", ""),
            ("", "", "Mesa Lavapipe"),
            ("Software Adapter", "", ""),
        ] {
            let mut info = wgpu::AdapterInfo::new(wgpu::DeviceType::Other, wgpu::Backend::Vulkan);
            info.name = name.to_owned();
            info.driver = driver.to_owned();
            info.driver_info = driver_info.to_owned();

            assert!(is_software_adapter(&info), "{info:?}");
        }
    }

    #[test]
    fn hardware_adapters_are_not_classified_as_software() {
        for device_type in [
            wgpu::DeviceType::DiscreteGpu,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::VirtualGpu,
            wgpu::DeviceType::Other,
        ] {
            let info = wgpu::AdapterInfo::new(device_type, wgpu::Backend::Vulkan);

            assert!(!is_software_adapter(&info), "{info:?}");
        }
    }
}
