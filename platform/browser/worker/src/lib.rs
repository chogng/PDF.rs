//! Browser Worker boundary primitives shared by native and `wasm32` builds.
//!
//! Browser control traffic uses the canonical 20-byte protocol header followed
//! by a `fixed_le_v1` binary payload. Browser objects remain outside that frame
//! in a resource table. Table entry zero is reserved for the transferred
//! control `ArrayBuffer`; protocol resource slot zero therefore maps to table
//! entry one. This crate validates pointer-free metadata only and never accepts
//! a raw Wasm pointer, `WebAssembly.Memory`, or DOM object.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod native_adapter;
mod wasm_mailbox;

pub use native_adapter::{
    BrowserNativeWorkerEvent, NativeBrowserWorker, NativeBrowserWorkerError,
    NativeBrowserWorkerPhase,
};
pub use wasm_mailbox::{NativeWorkerMailbox, NativeWorkerMailboxError};
#[cfg(target_arch = "wasm32")]
pub use wasm_mailbox::{
    wasm_dispatch, wasm_initialize, wasm_memory_epoch, wasm_output_length, wasm_output_pointer,
    wasm_poll, wasm_prepare_input, wasm_prepare_transfer, wasm_shutdown, wasm_transfer_count,
    wasm_transfer_length, wasm_transfer_pointer,
};

use pdf_rs_protocol::{
    ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER, ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER,
    ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP, ENVELOPE_HEADER_BYTES, MAX_MESSAGE_BYTES,
    MAX_TRANSFER_SLOTS, MIN_COMPATIBLE_MINOR, PROTOCOL_MAJOR, PROTOCOL_MINOR, SCHEMA_HASH,
};

/// The Rust target used for the browser Worker build.
pub const BROWSER_WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Resource-table index reserved for the binary control `ArrayBuffer`.
pub const CONTROL_RESOURCE_TABLE_INDEX: u16 = 0;

/// First resource-table index addressable by protocol resource slot zero.
pub const FIRST_PROTOCOL_RESOURCE_TABLE_INDEX: u16 = 1;

/// Width in bytes of the atomic SharedArrayBuffer publication fence.
pub const SHARED_PUBLICATION_FENCE_BYTES: u64 = 4;

/// Frozen protocol identity compiled into the browser Worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserWorkerProtocol {
    /// Supported protocol major version.
    pub major: u16,
    /// Supported protocol minor version.
    pub minor: u16,
    /// Oldest protocol minor accepted by this Worker.
    pub min_compatible_minor: u16,
    /// Truncated wire schema hash.
    pub schema_hash: [u8; 16],
    /// Canonical fixed header size for every binary control frame.
    pub control_header_bytes: usize,
    /// Maximum number of protocol OOB resource slots, excluding control entry zero.
    pub max_resource_slots: u16,
}

impl BrowserWorkerProtocol {
    /// Returns the protocol identity generated from the canonical schema.
    #[must_use]
    pub const fn generated() -> Self {
        Self {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            min_compatible_minor: MIN_COMPATIBLE_MINOR,
            schema_hash: SCHEMA_HASH,
            control_header_bytes: ENVELOPE_HEADER_BYTES,
            max_resource_slots: MAX_TRANSFER_SLOTS,
        }
    }
}

/// Negotiated limits and browser facts applied to one resource manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserIngressContext {
    negotiated_capabilities: u64,
    max_message_bytes: u32,
    max_resource_slots: u16,
    cross_origin_isolated: bool,
}

impl BrowserIngressContext {
    /// Creates a bounded context from an already validated browser handshake.
    pub fn negotiated(
        negotiated_capabilities: u64,
        max_message_bytes: u32,
        max_resource_slots: u16,
        cross_origin_isolated: bool,
    ) -> Result<Self, ResourceManifestError> {
        if max_message_bytes == 0
            || max_message_bytes > MAX_MESSAGE_BYTES
            || max_resource_slots == 0
            || max_resource_slots > MAX_TRANSFER_SLOTS
        {
            return Err(ResourceManifestError::InvalidConfiguration);
        }
        Ok(Self {
            negotiated_capabilities,
            max_message_bytes,
            max_resource_slots,
            cross_origin_isolated,
        })
    }

    /// Returns the endpoint capabilities negotiated by both peers.
    #[must_use]
    pub const fn negotiated_capabilities(self) -> u64 {
        self.negotiated_capabilities
    }

    /// Returns the negotiated maximum binary payload size.
    #[must_use]
    pub const fn max_message_bytes(self) -> u32 {
        self.max_message_bytes
    }

    /// Returns the negotiated maximum OOB resource count, excluding control.
    #[must_use]
    pub const fn max_resource_slots(self) -> u16 {
        self.max_resource_slots
    }

    /// Reports whether SharedArrayBuffer use is permitted by browser isolation.
    #[must_use]
    pub const fn cross_origin_isolated(self) -> bool {
        self.cross_origin_isolated
    }

    const fn supports(self, capability: u64) -> bool {
        self.negotiated_capabilities & capability == capability
    }
}

/// Declared and observed byte extent for an OOB buffer resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserByteExtent {
    /// Length declared by the protocol transport variant.
    pub declared_buffer_length: u64,
    /// Actual JavaScript object's `byteLength`.
    pub actual_buffer_length: u64,
    /// Start of the payload-bearing byte range in the resource.
    pub data_byte_offset: u64,
    /// Length of the payload-bearing byte range in the resource.
    pub data_byte_length: u64,
}

/// Pointer-free metadata for the binary control frame and browser OOB resources.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrowserResourceEntry {
    /// Reserved transferred control frame at resource-table index zero.
    Control {
        /// Resource-table index; it must be [`CONTROL_RESOURCE_TABLE_INDEX`].
        table_index: u16,
        /// Actual control `ArrayBuffer.byteLength`.
        actual_frame_length: u64,
        /// Payload length decoded from the canonical 20-byte header.
        declared_payload_length: u32,
        /// Whether the browser host placed the buffer in the transfer list.
        transferred: bool,
    },
    /// Transferred ArrayBuffer used by ProvideData or a browser Surface.
    ArrayBuffer {
        /// Index in the outer browser resource table.
        table_index: u16,
        /// Zero-based protocol resource slot.
        protocol_slot: u16,
        /// Declared and observed buffer extent.
        extent: BrowserByteExtent,
        /// Whether ownership moved through the browser transfer list.
        transferred: bool,
    },
    /// Transferred ImageBitmap used for Host-mediated Surface presentation.
    ImageBitmap {
        /// Index in the outer browser resource table.
        table_index: u16,
        /// Zero-based protocol resource slot.
        protocol_slot: u16,
        /// Width declared by the protocol Surface transport.
        declared_width: u32,
        /// Height declared by the protocol Surface transport.
        declared_height: u32,
        /// Actual JavaScript `ImageBitmap.width`.
        actual_width: u32,
        /// Actual JavaScript `ImageBitmap.height`.
        actual_height: u32,
        /// Whether ownership moved through the browser transfer list.
        transferred: bool,
    },
    /// SharedArrayBuffer attachment used for fenced Host-mediated presentation.
    SharedArrayBuffer {
        /// Index in the outer browser resource table.
        table_index: u16,
        /// Zero-based protocol attachment slot.
        protocol_slot: u16,
        /// Declared and observed buffer extent.
        extent: BrowserByteExtent,
        /// Actual JavaScript `SharedArrayBuffer.maxByteLength`.
        actual_max_byte_length: u64,
        /// Whether the actual SharedArrayBuffer reports itself growable.
        growable: bool,
        /// Byte offset of the aligned atomic publication fence.
        fence_byte_offset: u64,
        /// Publication epoch declared by the protocol transport.
        expected_publication_epoch: u32,
        /// Nonzero token that must remain owned until Surface release.
        release_token: u64,
        /// Epoch observed with `Atomics.load` before resource validation.
        observed_epoch_before: u32,
        /// Epoch observed with `Atomics.load` after resource validation.
        observed_epoch_after: u32,
    },
}

/// Stable, content-redacted manifest rejection categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceManifestError {
    /// Negotiated limits are zero or exceed generated protocol limits.
    InvalidConfiguration,
    /// Resource-table entry zero is absent or is not the control frame.
    MissingControlEntry,
    /// The reserved control entry has an invalid index, ownership, or length.
    InvalidControlEntry,
    /// The resource count exceeds the negotiated or generated slot limit.
    TooManyResources,
    /// Resource table indices and protocol slots are not contiguous with the reserved offset.
    NonContiguousResourceSlot,
    /// A resource kind was not enabled by negotiated endpoint capabilities.
    MissingCapability,
    /// A transferable resource was not marked as transferred.
    ResourceNotTransferred,
    /// Declared and actual resource dimensions or byte ranges disagree.
    InvalidExtent,
    /// SharedArrayBuffer was used without cross-origin isolation.
    SharedArrayBufferRequiresIsolation,
    /// SharedArrayBuffer is growable or its maximum and current lengths differ.
    SharedArrayBufferNotFixedLength,
    /// The SharedArrayBuffer fence is unaligned, out of bounds, or overlaps pixels.
    InvalidSharedPublicationFence,
    /// The SharedArrayBuffer release token is zero and cannot bind later release.
    InvalidSharedReleaseToken,
    /// The SharedArrayBuffer publication epoch was zero, stale, or changed during validation.
    SharedPublicationChanged,
}

/// Validates a complete pointer-free browser ingress resource manifest.
///
/// The first entry must describe the transferred binary control frame. Every
/// later entry maps protocol slot `n` to resource-table index `n + 1`.
/// Capability, extent, ownership, isolation, and SharedArrayBuffer publication
/// checks complete before the caller may adopt any OOB resource.
pub fn validate_resource_manifest(
    context: BrowserIngressContext,
    entries: &[BrowserResourceEntry],
) -> Result<(), ResourceManifestError> {
    let Some(BrowserResourceEntry::Control {
        table_index,
        actual_frame_length,
        declared_payload_length,
        transferred,
    }) = entries.first()
    else {
        return Err(ResourceManifestError::MissingControlEntry);
    };

    let expected_frame_length = u64::try_from(ENVELOPE_HEADER_BYTES)
        .ok()
        .and_then(|header| header.checked_add(u64::from(*declared_payload_length)));
    if *table_index != CONTROL_RESOURCE_TABLE_INDEX
        || !transferred
        || u64::from(*declared_payload_length) > u64::from(context.max_message_bytes)
        || expected_frame_length != Some(*actual_frame_length)
    {
        return Err(ResourceManifestError::InvalidControlEntry);
    }

    let resources = &entries[1..];
    if resources.len() > usize::from(context.max_resource_slots)
        || resources.len() > usize::from(MAX_TRANSFER_SLOTS)
    {
        return Err(ResourceManifestError::TooManyResources);
    }

    for (expected_slot, resource) in resources.iter().enumerate() {
        let expected_slot =
            u16::try_from(expected_slot).map_err(|_| ResourceManifestError::TooManyResources)?;
        validate_resource(context, resource, expected_slot)?;
    }
    Ok(())
}

fn validate_resource(
    context: BrowserIngressContext,
    resource: &BrowserResourceEntry,
    expected_slot: u16,
) -> Result<(), ResourceManifestError> {
    let expected_table_index = expected_slot
        .checked_add(FIRST_PROTOCOL_RESOURCE_TABLE_INDEX)
        .ok_or(ResourceManifestError::NonContiguousResourceSlot)?;
    match resource {
        BrowserResourceEntry::Control { .. } => Err(ResourceManifestError::InvalidControlEntry),
        BrowserResourceEntry::ArrayBuffer {
            table_index,
            protocol_slot,
            extent,
            transferred,
        } => {
            validate_position(
                *table_index,
                *protocol_slot,
                expected_table_index,
                expected_slot,
            )?;
            require_capability(context, ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER)?;
            if !transferred {
                return Err(ResourceManifestError::ResourceNotTransferred);
            }
            validate_byte_extent(*extent)
        }
        BrowserResourceEntry::ImageBitmap {
            table_index,
            protocol_slot,
            declared_width,
            declared_height,
            actual_width,
            actual_height,
            transferred,
        } => {
            validate_position(
                *table_index,
                *protocol_slot,
                expected_table_index,
                expected_slot,
            )?;
            require_capability(context, ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP)?;
            if !transferred {
                return Err(ResourceManifestError::ResourceNotTransferred);
            }
            if *declared_width == 0
                || *declared_height == 0
                || declared_width != actual_width
                || declared_height != actual_height
            {
                return Err(ResourceManifestError::InvalidExtent);
            }
            Ok(())
        }
        BrowserResourceEntry::SharedArrayBuffer {
            table_index,
            protocol_slot,
            extent,
            actual_max_byte_length,
            growable,
            fence_byte_offset,
            expected_publication_epoch,
            release_token,
            observed_epoch_before,
            observed_epoch_after,
        } => {
            validate_position(
                *table_index,
                *protocol_slot,
                expected_table_index,
                expected_slot,
            )?;
            require_capability(context, ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER)?;
            if !context.cross_origin_isolated {
                return Err(ResourceManifestError::SharedArrayBufferRequiresIsolation);
            }
            validate_byte_extent(*extent)?;
            if *growable
                || *actual_max_byte_length != extent.actual_buffer_length
                || *actual_max_byte_length != extent.declared_buffer_length
            {
                return Err(ResourceManifestError::SharedArrayBufferNotFixedLength);
            }
            validate_shared_fence(
                *extent,
                *fence_byte_offset,
                *expected_publication_epoch,
                *release_token,
                *observed_epoch_before,
                *observed_epoch_after,
            )
        }
    }
}

fn validate_position(
    table_index: u16,
    protocol_slot: u16,
    expected_table_index: u16,
    expected_slot: u16,
) -> Result<(), ResourceManifestError> {
    if table_index != expected_table_index || protocol_slot != expected_slot {
        return Err(ResourceManifestError::NonContiguousResourceSlot);
    }
    Ok(())
}

fn require_capability(
    context: BrowserIngressContext,
    capability: u64,
) -> Result<(), ResourceManifestError> {
    if !context.supports(capability) {
        return Err(ResourceManifestError::MissingCapability);
    }
    Ok(())
}

fn validate_byte_extent(extent: BrowserByteExtent) -> Result<(), ResourceManifestError> {
    let Some(data_end) = extent.data_byte_offset.checked_add(extent.data_byte_length) else {
        return Err(ResourceManifestError::InvalidExtent);
    };
    if extent.declared_buffer_length == 0
        || extent.actual_buffer_length != extent.declared_buffer_length
        || extent.data_byte_length == 0
        || data_end > extent.actual_buffer_length
    {
        return Err(ResourceManifestError::InvalidExtent);
    }
    Ok(())
}

fn validate_shared_fence(
    extent: BrowserByteExtent,
    fence_byte_offset: u64,
    expected_publication_epoch: u32,
    release_token: u64,
    observed_epoch_before: u32,
    observed_epoch_after: u32,
) -> Result<(), ResourceManifestError> {
    let Some(fence_end) = fence_byte_offset.checked_add(SHARED_PUBLICATION_FENCE_BYTES) else {
        return Err(ResourceManifestError::InvalidSharedPublicationFence);
    };
    let data_end = extent
        .data_byte_offset
        .checked_add(extent.data_byte_length)
        .ok_or(ResourceManifestError::InvalidExtent)?;
    let overlaps_data = fence_byte_offset < data_end && extent.data_byte_offset < fence_end;
    if !fence_byte_offset.is_multiple_of(SHARED_PUBLICATION_FENCE_BYTES)
        || fence_end > extent.actual_buffer_length
        || overlaps_data
    {
        return Err(ResourceManifestError::InvalidSharedPublicationFence);
    }
    if release_token == 0 {
        return Err(ResourceManifestError::InvalidSharedReleaseToken);
    }
    if expected_publication_epoch == 0
        || observed_epoch_before != expected_publication_epoch
        || observed_epoch_after != expected_publication_epoch
    {
        return Err(ResourceManifestError::SharedPublicationChanged);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_BROWSER_RESOURCES: u64 = ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER
        | ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP
        | ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER;

    fn context(capabilities: u64, cross_origin_isolated: bool) -> BrowserIngressContext {
        BrowserIngressContext::negotiated(
            capabilities,
            MAX_MESSAGE_BYTES,
            MAX_TRANSFER_SLOTS,
            cross_origin_isolated,
        )
        .unwrap()
    }

    const fn control() -> BrowserResourceEntry {
        control_entry(
            CONTROL_RESOURCE_TABLE_INDEX,
            ENVELOPE_HEADER_BYTES as u64 + 8,
            true,
        )
    }

    const fn control_entry(
        table_index: u16,
        actual_frame_length: u64,
        transferred: bool,
    ) -> BrowserResourceEntry {
        BrowserResourceEntry::Control {
            table_index,
            actual_frame_length,
            declared_payload_length: 8,
            transferred,
        }
    }

    const fn byte_extent() -> BrowserByteExtent {
        BrowserByteExtent {
            declared_buffer_length: 64,
            actual_buffer_length: 64,
            data_byte_offset: 0,
            data_byte_length: 32,
        }
    }

    const fn array_buffer(table_index: u16, protocol_slot: u16) -> BrowserResourceEntry {
        array_buffer_entry(table_index, protocol_slot, byte_extent(), true)
    }

    const fn array_buffer_entry(
        table_index: u16,
        protocol_slot: u16,
        extent: BrowserByteExtent,
        transferred: bool,
    ) -> BrowserResourceEntry {
        BrowserResourceEntry::ArrayBuffer {
            table_index,
            protocol_slot,
            extent,
            transferred,
        }
    }

    const fn image_bitmap(table_index: u16, protocol_slot: u16) -> BrowserResourceEntry {
        image_bitmap_entry(table_index, protocol_slot, 16, 9, 16, 9, true)
    }

    const fn image_bitmap_entry(
        table_index: u16,
        protocol_slot: u16,
        declared_width: u32,
        declared_height: u32,
        actual_width: u32,
        actual_height: u32,
        transferred: bool,
    ) -> BrowserResourceEntry {
        BrowserResourceEntry::ImageBitmap {
            table_index,
            protocol_slot,
            declared_width,
            declared_height,
            actual_width,
            actual_height,
            transferred,
        }
    }

    const fn shared_array_buffer(table_index: u16, protocol_slot: u16) -> BrowserResourceEntry {
        shared_array_buffer_entry(
            table_index,
            protocol_slot,
            byte_extent(),
            64,
            false,
            32,
            7,
            11,
            7,
            7,
        )
    }

    #[allow(clippy::too_many_arguments)]
    const fn shared_array_buffer_entry(
        table_index: u16,
        protocol_slot: u16,
        extent: BrowserByteExtent,
        actual_max_byte_length: u64,
        growable: bool,
        fence_byte_offset: u64,
        expected_publication_epoch: u32,
        release_token: u64,
        observed_epoch_before: u32,
        observed_epoch_after: u32,
    ) -> BrowserResourceEntry {
        BrowserResourceEntry::SharedArrayBuffer {
            table_index,
            protocol_slot,
            extent,
            actual_max_byte_length,
            growable,
            fence_byte_offset,
            expected_publication_epoch,
            release_token,
            observed_epoch_before,
            observed_epoch_after,
        }
    }

    #[test]
    fn generated_protocol_identity_includes_binary_header_and_resource_cap() {
        let protocol = BrowserWorkerProtocol::generated();
        assert_eq!(protocol.major, PROTOCOL_MAJOR);
        assert_eq!(protocol.minor, PROTOCOL_MINOR);
        assert_eq!(protocol.min_compatible_minor, MIN_COMPATIBLE_MINOR);
        assert_eq!(protocol.schema_hash, SCHEMA_HASH);
        assert_eq!(protocol.control_header_bytes, ENVELOPE_HEADER_BYTES);
        assert_eq!(protocol.max_resource_slots, MAX_TRANSFER_SLOTS);
    }

    #[test]
    fn accepts_reserved_control_and_three_host_mediated_resource_kinds() {
        let entries = [
            control(),
            array_buffer(1, 0),
            image_bitmap(2, 1),
            shared_array_buffer(3, 2),
        ];
        assert_eq!(
            validate_resource_manifest(context(ALL_BROWSER_RESOURCES, true), &entries),
            Ok(())
        );
    }

    #[test]
    fn configuration_and_reserved_control_are_fail_closed() {
        for invalid in [
            BrowserIngressContext::negotiated(0, 0, 1, false),
            BrowserIngressContext::negotiated(0, MAX_MESSAGE_BYTES + 1, 1, false),
            BrowserIngressContext::negotiated(0, 1, 0, false),
            BrowserIngressContext::negotiated(0, 1, MAX_TRANSFER_SLOTS + 1, false),
        ] {
            assert_eq!(invalid, Err(ResourceManifestError::InvalidConfiguration));
        }

        assert_eq!(
            validate_resource_manifest(context(0, false), &[]),
            Err(ResourceManifestError::MissingControlEntry)
        );
        let missing_control = [array_buffer(1, 0)];
        assert_eq!(
            validate_resource_manifest(context(ALL_BROWSER_RESOURCES, true), &missing_control),
            Err(ResourceManifestError::MissingControlEntry)
        );
        for invalid_control in [
            control_entry(1, ENVELOPE_HEADER_BYTES as u64 + 8, true),
            control_entry(
                CONTROL_RESOURCE_TABLE_INDEX,
                ENVELOPE_HEADER_BYTES as u64 + 7,
                true,
            ),
            control_entry(
                CONTROL_RESOURCE_TABLE_INDEX,
                ENVELOPE_HEADER_BYTES as u64 + 8,
                false,
            ),
        ] {
            assert_eq!(
                validate_resource_manifest(context(0, false), &[invalid_control]),
                Err(ResourceManifestError::InvalidControlEntry)
            );
        }

        let strict_bytes = BrowserIngressContext::negotiated(0, 7, 1, false).unwrap();
        assert_eq!(
            validate_resource_manifest(strict_bytes, &[control()]),
            Err(ResourceManifestError::InvalidControlEntry)
        );

        let one_resource =
            BrowserIngressContext::negotiated(ALL_BROWSER_RESOURCES, MAX_MESSAGE_BYTES, 1, false)
                .unwrap();
        assert_eq!(
            validate_resource_manifest(
                one_resource,
                &[control(), array_buffer(1, 0), image_bitmap(2, 1)]
            ),
            Err(ResourceManifestError::TooManyResources)
        );
    }

    #[test]
    fn resource_slots_start_after_control_and_remain_contiguous() {
        for entries in [
            vec![control(), array_buffer(2, 0)],
            vec![control(), array_buffer(1, 1)],
            vec![control(), control()],
        ] {
            assert!(matches!(
                validate_resource_manifest(context(ALL_BROWSER_RESOURCES, true), &entries),
                Err(ResourceManifestError::NonContiguousResourceSlot
                    | ResourceManifestError::InvalidControlEntry)
            ));
        }

        let exact_resources = usize::from(MAX_TRANSFER_SLOTS);
        let mut exact = Vec::with_capacity(exact_resources + 1);
        exact.push(control());
        exact.extend((0..exact_resources).map(|slot| {
            let slot = u16::try_from(slot).unwrap();
            array_buffer(slot + FIRST_PROTOCOL_RESOURCE_TABLE_INDEX, slot)
        }));
        assert_eq!(
            validate_resource_manifest(context(ALL_BROWSER_RESOURCES, true), &exact),
            Ok(())
        );

        let mut too_many = exact;
        too_many.push(array_buffer(
            MAX_TRANSFER_SLOTS + FIRST_PROTOCOL_RESOURCE_TABLE_INDEX,
            MAX_TRANSFER_SLOTS,
        ));
        assert_eq!(
            validate_resource_manifest(context(ALL_BROWSER_RESOURCES, true), &too_many),
            Err(ResourceManifestError::TooManyResources)
        );
    }

    #[test]
    fn every_resource_requires_its_negotiated_capability_and_transfer_mode() {
        for resource in [
            array_buffer(1, 0),
            image_bitmap(1, 0),
            shared_array_buffer(1, 0),
        ] {
            assert_eq!(
                validate_resource_manifest(context(0, true), &[control(), resource]),
                Err(ResourceManifestError::MissingCapability)
            );
        }

        for resource in [
            array_buffer_entry(1, 0, byte_extent(), false),
            image_bitmap_entry(1, 0, 16, 9, 16, 9, false),
        ] {
            assert_eq!(
                validate_resource_manifest(
                    context(ALL_BROWSER_RESOURCES, true),
                    &[control(), resource]
                ),
                Err(ResourceManifestError::ResourceNotTransferred)
            );
        }
    }

    #[test]
    fn buffer_and_bitmap_extents_must_match_actual_browser_objects() {
        for resource in [
            array_buffer_entry(
                1,
                0,
                BrowserByteExtent {
                    actual_buffer_length: 63,
                    ..byte_extent()
                },
                true,
            ),
            array_buffer_entry(
                1,
                0,
                BrowserByteExtent {
                    data_byte_offset: u64::MAX,
                    data_byte_length: 2,
                    ..byte_extent()
                },
                true,
            ),
            image_bitmap_entry(1, 0, 16, 9, 15, 9, true),
            image_bitmap_entry(1, 0, 16, 0, 16, 0, true),
        ] {
            assert_eq!(
                validate_resource_manifest(
                    context(ALL_BROWSER_RESOURCES, true),
                    &[control(), resource]
                ),
                Err(ResourceManifestError::InvalidExtent)
            );
        }
    }

    #[test]
    fn shared_array_buffer_requires_isolation_fixed_length_and_stable_fence() {
        let shared = shared_array_buffer(1, 0);
        assert_eq!(
            validate_resource_manifest(context(ALL_BROWSER_RESOURCES, false), &[control(), shared]),
            Err(ResourceManifestError::SharedArrayBufferRequiresIsolation)
        );

        for resource in [
            shared_array_buffer_entry(1, 0, byte_extent(), 64, true, 32, 7, 11, 7, 7),
            shared_array_buffer_entry(1, 0, byte_extent(), 65, false, 32, 7, 11, 7, 7),
        ] {
            assert_eq!(
                validate_resource_manifest(
                    context(ALL_BROWSER_RESOURCES, true),
                    &[control(), resource]
                ),
                Err(ResourceManifestError::SharedArrayBufferNotFixedLength)
            );
        }

        for resource in [
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 31, 7, 11, 7, 7),
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 64, 7, 11, 7, 7),
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 28, 7, 11, 7, 7),
        ] {
            assert_eq!(
                validate_resource_manifest(
                    context(ALL_BROWSER_RESOURCES, true),
                    &[control(), resource]
                ),
                Err(ResourceManifestError::InvalidSharedPublicationFence)
            );
        }

        let missing_release =
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 32, 7, 0, 7, 7);
        assert_eq!(
            validate_resource_manifest(
                context(ALL_BROWSER_RESOURCES, true),
                &[control(), missing_release]
            ),
            Err(ResourceManifestError::InvalidSharedReleaseToken)
        );

        for resource in [
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 32, 0, 11, 0, 0),
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 32, 7, 11, 6, 7),
            shared_array_buffer_entry(1, 0, byte_extent(), 64, false, 32, 7, 11, 7, 8),
        ] {
            assert_eq!(
                validate_resource_manifest(
                    context(ALL_BROWSER_RESOURCES, true),
                    &[control(), resource]
                ),
                Err(ResourceManifestError::SharedPublicationChanged)
            );
        }
    }
}
