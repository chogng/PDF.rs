use crate::{
    BaselineChannel, BaselineDescriptor, BaselineError, BaselineObservation, BaselineRequest,
    BaselineRunner, ProcessBaselineRunner, ProcessLimits, ProcessSpec, invalid_process_config,
    malformed_response, output_limit,
};

/// Adapter profile for a direct PDFium public-C-API helper that only emits pixels.
pub const PDFIUM_PIXEL_ADAPTER_PROFILE: &str = "pdfium-public-c-api-pixel-only-v1";

/// Hard RGBA payload ceiling for one PDFium pixel-profile observation.
pub const PDFIUM_PIXEL_ADAPTER_MAX_RGBA_BYTES: u64 = 64 * 1024 * 1024;

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
    limits: ProcessLimits,
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
            || !process.environment_is_empty()
            || process.executable_sha256()? != descriptor.build_hash
        {
            return Err(invalid_process_config());
        }
        Ok(Self {
            inner: ProcessBaselineRunner::new(descriptor, process, limits)?,
            limits,
        })
    }
}

impl BaselineRunner for PdfiumPixelAdapter {
    fn describe(&self) -> Result<BaselineDescriptor, BaselineError> {
        self.inner.describe()
    }

    fn observe(&self, request: &BaselineRequest) -> Result<BaselineObservation, BaselineError> {
        let rgba_bytes = u64::from(request.width())
            .checked_mul(u64::from(request.height()))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(output_limit)?;
        let response_bytes = u64::try_from(crate::RESPONSE_HEADER_LEN)
            .map_err(|_| output_limit())?
            .checked_add(rgba_bytes)
            .ok_or_else(output_limit)?;
        if rgba_bytes > PDFIUM_PIXEL_ADAPTER_MAX_RGBA_BYTES
            || response_bytes > self.limits.max_stdout_bytes()
        {
            return Err(output_limit());
        }
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
