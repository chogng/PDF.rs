use pdf_rs_protocol::{
    DESKTOP_FRAME_HEADER_BYTES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, ProtocolErrorCode,
    ProtocolLimitConfig, ProtocolLimits,
};

fn config() -> ProtocolLimitConfig {
    ProtocolLimitConfig::default()
}

#[test]
fn default_limits_cover_the_generated_protocol_bounds() {
    let limits = ProtocolLimits::default();
    assert_eq!(limits.max_payload_bytes(), MAX_MESSAGE_BYTES);
    assert_eq!(limits.max_transfer_slots(), MAX_TRANSFER_SLOTS);
    assert_eq!(
        limits.max_frame_bytes(),
        u64::try_from(DESKTOP_FRAME_HEADER_BYTES).unwrap() + u64::from(MAX_MESSAGE_BYTES)
    );
    assert!(limits.max_surface_dimension() > 0);
    assert!(limits.max_surface_stride_bytes() > 0);
    assert!(limits.max_surface_bytes() >= limits.max_surface_stride_bytes());
}

#[test]
fn every_zero_and_inconsistent_limit_fails_closed() {
    let mut cases = Vec::new();
    let mut value = config();
    value.max_frame_bytes = 0;
    cases.push(value);
    let mut value = config();
    value.max_payload_bytes = 0;
    cases.push(value);
    let mut value = config();
    value.max_transfer_slots = 0;
    cases.push(value);
    let mut value = config();
    value.max_surface_dimension = 0;
    cases.push(value);
    let mut value = config();
    value.max_surface_stride_bytes = 0;
    cases.push(value);
    let mut value = config();
    value.max_surface_bytes = 0;
    cases.push(value);
    let mut value = config();
    value.max_frame_bytes =
        u64::try_from(DESKTOP_FRAME_HEADER_BYTES).unwrap() + u64::from(value.max_payload_bytes) - 1;
    cases.push(value);

    for invalid in cases {
        let error = ProtocolLimits::new(invalid).unwrap_err();
        assert_eq!(error.code(), ProtocolErrorCode::InvalidLimits);
        assert_eq!(error.diagnostic_id(), "RPE-PROTOCOL-0001");
    }
}

#[test]
fn errors_are_stable_and_payload_and_handle_redacted() {
    let error = ProtocolLimits::new(ProtocolLimitConfig {
        max_frame_bytes: 0,
        ..config()
    })
    .unwrap_err();
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert!(debug.contains("RPE-PROTOCOL-0001"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("PlatformHandle("));
    assert_eq!(
        display,
        "RPE-PROTOCOL-0001: rejected Native Engine protocol input"
    );
}
