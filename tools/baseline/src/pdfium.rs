use crate::{
    BaselineChannel, BaselineDescriptor, BaselineError, BaselineObservation, BaselineRequest,
    BaselineRunner, ProcessBaselineRunner, ProcessLimits, ProcessSpec, invalid_process_config,
    malformed_response,
};

/// Adapter profile for a direct PDFium public-C-API helper that only emits pixels.
pub const PDFIUM_PIXEL_ADAPTER_PROFILE: &str = "pdfium-public-c-api-pixel-only-v1";

/// Profile-enforcing host wrapper for the direct PDFium pixel adapter process.
///
/// The wrapped executable must itself speak baseline protocol schema 2 and link
/// PDFium directly. It receives no command-line arguments, preventing this type
/// from silently becoming a wrapper around stock `pdfium_test`. Parse, Scene,
/// and positioned-text channels must be explicitly unsupported; pixels must be
/// produced or explicitly failed.
///
/// This type does not add a platform sandbox. The caller must still arrange and
/// fingerprint descendant, CPU, memory, filesystem, syscall, and network
/// containment before registering a real baseline.
pub struct PdfiumPixelAdapter {
    inner: ProcessBaselineRunner,
}

impl PdfiumPixelAdapter {
    /// Binds a complete PDFium descriptor to a no-argument direct helper process.
    pub fn new(
        descriptor: BaselineDescriptor,
        process: ProcessSpec,
        limits: ProcessLimits,
    ) -> Result<Self, BaselineError> {
        if descriptor.id != PDFIUM_PIXEL_ADAPTER_PROFILE
            || descriptor.engine != "pdfium"
            || !process.arguments_are_empty()
        {
            return Err(invalid_process_config());
        }
        Ok(Self {
            inner: ProcessBaselineRunner::new(descriptor, process, limits)?,
        })
    }
}

impl BaselineRunner for PdfiumPixelAdapter {
    fn describe(&self) -> Result<BaselineDescriptor, BaselineError> {
        self.inner.describe()
    }

    fn observe(&self, request: &BaselineRequest) -> Result<BaselineObservation, BaselineError> {
        let observation = self.inner.observe(request)?;
        if !matches!(observation.parse_json, BaselineChannel::Unsupported)
            || !matches!(observation.scene_json, BaselineChannel::Unsupported)
            || !matches!(observation.text_json, BaselineChannel::Unsupported)
            || matches!(observation.rgba, BaselineChannel::Unsupported)
        {
            return Err(malformed_response());
        }
        Ok(observation)
    }
}
