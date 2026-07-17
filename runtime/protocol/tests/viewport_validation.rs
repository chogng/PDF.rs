use pdf_rs_protocol::{
    Correlation, OutputProfile, PageCoordinateSpace, PageGeometry, PageRotation, PageViewport,
    ProtocolErrorCode, ProtocolLimits, ProtocolValidator, QualityPolicy, SessionId,
    SetViewportCommand, VIEWPORT_REQUEST_VISIBLE_PAGES_MAX_COUNT, ViewportRequest, WorkerId,
};

fn page(page_index: u32) -> PageViewport {
    PageViewport {
        page_index,
        coordinate_space: PageCoordinateSpace::PdfPointsBottomLeft,
        geometry: PageGeometry {
            identity: [page_index.saturating_add(1) as u8; 32],
            media_box_x_milli_points: 0,
            media_box_y_milli_points: 0,
            media_box_width_milli_points: 612_000,
            media_box_height_milli_points: 792_000,
            crop_box_x_milli_points: 0,
            crop_box_y_milli_points: 0,
            crop_box_width_milli_points: 612_000,
            crop_box_height_milli_points: 792_000,
            intrinsic_rotation: PageRotation::Degrees0,
        },
        clip_x_milli_points: 0,
        clip_y_milli_points: 0,
        clip_width_milli_points: 612_000,
        clip_height_milli_points: 792_000,
    }
}

fn viewport() -> ViewportRequest {
    ViewportRequest {
        generation: 7,
        document_revision: 3,
        annotation_revision: 0,
        zoom_numerator: 3,
        zoom_denominator: 2,
        visible_pages: vec![page(0), page(1)],
        quality: QualityPolicy::Full,
        output_profile: OutputProfile::Srgb,
        device_scale_milli: 2_000,
        rotation: PageRotation::Degrees0,
        optional_content_id: 0,
    }
}

#[test]
fn set_viewport_binds_exact_generation_worker_and_session() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let worker = WorkerId::new(11);
    let session = SessionId::new(13);
    let command = SetViewportCommand {
        viewport: viewport(),
    };
    let correlation = Correlation {
        worker,
        session: Some(session),
        request: None,
        generation: Some(command.viewport.generation),
    };
    validator
        .validate_set_viewport(&correlation, &command, worker, session)
        .unwrap();

    let mut mismatched = correlation;
    mismatched.generation = Some(command.viewport.generation + 1);
    assert_eq!(
        validator
            .validate_set_viewport(&mismatched, &command, worker, session)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidCorrelation
    );
}

#[test]
fn viewport_zero_noncanonical_duplicate_and_limit_boundaries_fail_closed() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let original = viewport();
    let mut invalid = Vec::new();

    let mut value = original.clone();
    value.generation = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.document_revision = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.zoom_numerator = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.zoom_denominator = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.zoom_numerator = 6;
    value.zoom_denominator = 4;
    invalid.push(value);
    let mut value = original.clone();
    value.device_scale_milli = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.visible_pages[1].page_index = value.visible_pages[0].page_index;
    invalid.push(value);
    let mut value = original.clone();
    value.visible_pages[1].geometry.identity = value.visible_pages[0].geometry.identity;
    invalid.push(value);
    let mut value = original.clone();
    value.visible_pages[0].geometry.identity = [0; 32];
    invalid.push(value);
    let mut value = original.clone();
    value.visible_pages[0].geometry.media_box_width_milli_points = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.visible_pages[0].geometry.crop_box_height_milli_points = 0;
    invalid.push(value);
    let mut value = original.clone();
    value.visible_pages[0].clip_width_milli_points = 0;
    invalid.push(value);
    let mut value = original;
    value.visible_pages = (0..=VIEWPORT_REQUEST_VISIBLE_PAGES_MAX_COUNT)
        .map(|index| page(u32::try_from(index).unwrap()))
        .collect();
    invalid.push(value);

    for value in invalid {
        assert_eq!(
            validator
                .validate_viewport_request(&value)
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidViewport
        );
    }
}

#[test]
fn exact_visible_page_limit_and_empty_viewport_are_deterministic() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let mut exact = viewport();
    exact.visible_pages = (0..VIEWPORT_REQUEST_VISIBLE_PAGES_MAX_COUNT)
        .map(|index| page(u32::try_from(index).unwrap()))
        .collect();
    validator.validate_viewport_request(&exact).unwrap();

    exact.visible_pages.clear();
    validator.validate_viewport_request(&exact).unwrap();
}
