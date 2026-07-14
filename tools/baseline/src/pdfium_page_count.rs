use crate::{
    BaselineChannel, BaselineDescriptor, BaselineError, BaselineObservation, BaselineRequest,
    BaselineRunner, ProcessBaselineRunner, ProcessLimits, ProcessSpec, invalid_process_config,
    invalid_request, malformed_response, output_limit,
};

/// Adapter profile for a direct PDFium public-C-API helper that emits a page count.
pub const PDFIUM_PAGE_COUNT_ADAPTER_PROFILE: &str = "pdfium-public-c-api-page-count-v1";

/// Hard canonical JSON ceiling for one PDFium page-count observation.
pub const PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES: u64 = 64;

/// Profile-enforcing host wrapper for the direct PDFium page-count probe.
///
/// The helper emits one bounded canonical JSON document through the parse
/// channel. Scene, positioned text, and pixels must be explicitly unsupported.
/// The request uses fixed metadata geometry because page count is a
/// document-level observation rather than a page-render request.
///
/// This type does not add a platform sandbox. The caller must still arrange
/// reviewed containment and complete fingerprints before baseline registration.
pub struct PdfiumPageCountAdapter {
    inner: ProcessBaselineRunner,
}

impl PdfiumPageCountAdapter {
    /// Binds a complete PDFium descriptor to a no-argument page-count helper process.
    pub fn new(
        descriptor: BaselineDescriptor,
        process: ProcessSpec,
        limits: ProcessLimits,
    ) -> Result<Self, BaselineError> {
        let maximum_response = u64::try_from(crate::RESPONSE_HEADER_LEN)
            .map_err(|_| invalid_process_config())?
            .checked_add(PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES)
            .ok_or_else(invalid_process_config)?;
        if descriptor.id != PDFIUM_PAGE_COUNT_ADAPTER_PROFILE
            || descriptor.engine != "pdfium"
            || !process.arguments_are_empty()
            || !process.environment_is_empty()
            || process.executable_sha256()? != descriptor.build_hash
            || limits.max_stdout_bytes() > maximum_response
        {
            return Err(invalid_process_config());
        }
        Ok(Self {
            inner: ProcessBaselineRunner::new(descriptor, process, limits)?,
        })
    }
}

impl BaselineRunner for PdfiumPageCountAdapter {
    fn describe(&self) -> Result<BaselineDescriptor, BaselineError> {
        self.inner.describe()
    }

    fn observe(&self, request: &BaselineRequest) -> Result<BaselineObservation, BaselineError> {
        if request.page() != 0 || request.width() != 1 || request.height() != 1 {
            return Err(invalid_request());
        }
        let observation = self.inner.observe(request)?;
        let parse_json = match &observation.parse_json {
            BaselineChannel::Produced(value)
                if u64::try_from(value.len()).map_err(|_| output_limit())?
                    <= PDFIUM_PAGE_COUNT_ADAPTER_MAX_PARSE_BYTES
                    && value.last() == Some(&b'\n')
                    && std::str::from_utf8(value).is_ok() =>
            {
                value
            }
            _ => return Err(malformed_response()),
        };
        debug_assert!(!parse_json.is_empty());
        if !matches!(observation.scene_json, BaselineChannel::Unsupported)
            || !matches!(observation.text_json, BaselineChannel::Unsupported)
            || !matches!(observation.rgba, BaselineChannel::Unsupported)
        {
            return Err(malformed_response());
        }
        Ok(observation)
    }
}
