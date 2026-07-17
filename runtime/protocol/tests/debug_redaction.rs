use pdf_rs_protocol::{
    ByteRange, DataSegment, DataTicket, PlatformHandle, ProvideDataCommand, SourceIdentity,
    SurfaceTransport,
};

#[test]
fn generated_debug_redacts_platform_handles_and_sensitive_union_fields() {
    let handle = PlatformHandle::new(3_735_928_559);
    let transport = SurfaceTransport::SharedMemory {
        handle,
        region_length: 4_294_967_291,
        release_token: 4_026_531_841,
    };

    for debug in [format!("{handle:?}"), format!("{transport:?}")] {
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("3735928559"));
        assert!(!debug.contains("4026531841"));
    }
}

#[test]
fn generated_debug_recursively_redacts_private_and_sensitive_record_fields() {
    let source = SourceIdentity {
        stable_id: [0xab; 32],
        revision: 7,
    };
    let command = ProvideDataCommand {
        ticket: DataTicket::new(11),
        source,
        segments: vec![DataSegment {
            range: ByteRange { start: 13, len: 17 },
            slot: 0,
            byte_length: 17,
        }],
    };
    let debug = format!("{command:?}");

    assert!(debug.contains("ticket"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("171"));
    assert!(!debug.contains("start: 13"));
    assert!(!debug.contains("byte_length: 17"));
}
