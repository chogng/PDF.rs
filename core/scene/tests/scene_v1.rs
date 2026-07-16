use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_scene::{
    CommandSource, Matrix, PageGeometry, PageRotation, SceneBinding, SceneBuilder, SceneErrorCode,
    SceneFeature, SceneLimitConfig, SceneLimitKind, SceneLimits, SceneRect, SceneScalar,
};
use pdf_rs_syntax::ObjectRef;

fn source(salt: u8) -> SourceIdentity {
    SourceIdentity::new(
        SourceStableId::new([salt; 32]),
        SourceRevision::new(u64::from(salt) + 1),
    )
}

fn scalar(value: &str) -> SceneScalar {
    SceneScalar::from_decimal(value).expect("test decimal fits Scene fixed point")
}

fn geometry() -> PageGeometry {
    let media = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar("100"),
        scalar("200"),
    ])
    .unwrap();
    PageGeometry::new(media, media, PageRotation::Degrees0)
}

fn binding(salt: u8) -> SceneBinding {
    SceneBinding::new(source(salt), 42, 0, ObjectRef::new(3, 0).unwrap())
}

fn command_source(operator_index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(4, 0).unwrap(),
        0,
        u64::from(operator_index) * 4,
        3,
        operator_index,
    )
    .unwrap()
}

fn limits(mut update: impl FnMut(&mut SceneLimitConfig)) -> SceneLimits {
    let mut config = SceneLimitConfig::default();
    update(&mut config);
    SceneLimits::validate(config).unwrap()
}

#[test]
fn empty_scene_has_exact_stable_field_order() {
    let scene = SceneBuilder::new(binding(1), geometry(), SceneLimits::default())
        .finish()
        .unwrap();
    assert_eq!(scene.version().major(), 1);
    assert_eq!(scene.version().minor(), 0);
    assert!(scene.commands().is_empty());
    assert!(scene.resources().is_empty());
    assert!(scene.provenance().is_empty());
    assert!(scene.features().tags().is_empty());
    assert_eq!(scene.stats().retained_bytes(), 0);

    let canonical = scene.canonical_json_bytes().unwrap();
    assert_eq!(
        std::str::from_utf8(&canonical).unwrap(),
        "{\"binding\":{\"page_index\":0,\"page_object\":{\"generation\":0,\"number\":3},\"revision_startxref\":42},\"commands\":[],\"features\":{\"decision\":\"supported\",\"tags\":[]},\"geometry\":{\"crop_box\":[0,0,100000000000,200000000000],\"media_box\":[0,0,100000000000,200000000000],\"rotation\":0},\"provenance\":[],\"resources\":[],\"schema\":{\"major\":1,\"minor\":0}}"
    );
}

#[test]
fn runtime_source_identity_is_omitted_from_canonical_semantics() {
    let first = SceneBuilder::new(binding(2), geometry(), SceneLimits::default())
        .finish()
        .unwrap();
    let second = SceneBuilder::new(binding(91), geometry(), SceneLimits::default())
        .finish()
        .unwrap();
    assert_ne!(first.binding().source(), second.binding().source());
    assert_eq!(
        first.canonical_json_bytes().unwrap(),
        second.canonical_json_bytes().unwrap()
    );
}

#[test]
fn marked_content_resources_and_features_are_stable_by_first_command_use() {
    let properties = ObjectRef::new(10, 0).unwrap();
    let mut builder = SceneBuilder::new(binding(3), geometry(), SceneLimits::default());
    builder
        .begin_marked_content(b"Span", Some(properties), command_source(0))
        .unwrap();
    builder.end_marked_content(command_source(1)).unwrap();
    builder
        .begin_marked_content(b"Artifact", Some(properties), command_source(2))
        .unwrap();
    builder.end_marked_content(command_source(3)).unwrap();
    let scene = builder.finish().unwrap();

    assert_eq!(scene.resources().len(), 1);
    assert_eq!(scene.resources()[0].id().value(), 0);
    assert_eq!(scene.resources()[0].object(), properties);
    assert_eq!(
        scene.features().tags(),
        &[
            SceneFeature::MarkedContent,
            SceneFeature::MarkedContentProperties
        ]
    );
    assert_eq!(scene.commands()[0].properties().unwrap().value(), 0);
    assert_eq!(scene.commands()[2].properties().unwrap().value(), 0);
    assert_eq!(scene.provenance()[2].operator_index(), 2);
    assert_eq!(scene.stats().max_marked_content_depth(), 1);

    let canonical = scene.canonical_json_bytes().unwrap();
    let canonical = std::str::from_utf8(&canonical).unwrap();
    assert_eq!(
        canonical,
        "{\"binding\":{\"page_index\":0,\"page_object\":{\"generation\":0,\"number\":3},\"revision_startxref\":42},\"commands\":[{\"kind\":\"begin-marked-content\",\"properties\":0,\"tag_hex\":\"5370616e\"},{\"kind\":\"end-marked-content\"},{\"kind\":\"begin-marked-content\",\"properties\":0,\"tag_hex\":\"4172746966616374\"},{\"kind\":\"end-marked-content\"}],\"features\":{\"decision\":\"supported\",\"tags\":[\"marked-content\",\"marked-content-properties\"]},\"geometry\":{\"crop_box\":[0,0,100000000000,200000000000],\"media_box\":[0,0,100000000000,200000000000],\"rotation\":0},\"provenance\":[{\"decoded_length\":3,\"decoded_start\":0,\"object\":{\"generation\":0,\"number\":4},\"operator_index\":0,\"stream_index\":0},{\"decoded_length\":3,\"decoded_start\":4,\"object\":{\"generation\":0,\"number\":4},\"operator_index\":1,\"stream_index\":0},{\"decoded_length\":3,\"decoded_start\":8,\"object\":{\"generation\":0,\"number\":4},\"operator_index\":2,\"stream_index\":0},{\"decoded_length\":3,\"decoded_start\":12,\"object\":{\"generation\":0,\"number\":4},\"operator_index\":3,\"stream_index\":0}],\"resources\":[{\"id\":0,\"kind\":\"marked-content-properties\",\"object\":{\"generation\":0,\"number\":10}}],\"schema\":{\"major\":1,\"minor\":0}}"
    );

    let mut replay = SceneBuilder::new(binding(3), geometry(), SceneLimits::default());
    replay
        .begin_marked_content(b"Span", Some(properties), command_source(0))
        .unwrap();
    replay.end_marked_content(command_source(1)).unwrap();
    replay
        .begin_marked_content(b"Artifact", Some(properties), command_source(2))
        .unwrap();
    replay.end_marked_content(command_source(3)).unwrap();
    assert_eq!(
        scene.canonical_json_bytes().unwrap(),
        replay.finish().unwrap().canonical_json_bytes().unwrap()
    );
}

#[test]
fn distinct_resources_follow_first_command_use_instead_of_object_order() {
    let first = ObjectRef::new(20, 0).unwrap();
    let second = ObjectRef::new(10, 0).unwrap();
    let mut builder = SceneBuilder::new(binding(12), geometry(), SceneLimits::default());
    builder
        .begin_marked_content(b"First", Some(first), command_source(0))
        .unwrap();
    builder.end_marked_content(command_source(1)).unwrap();
    builder
        .begin_marked_content(b"Second", Some(second), command_source(2))
        .unwrap();
    builder.end_marked_content(command_source(3)).unwrap();
    builder
        .begin_marked_content(b"FirstAgain", Some(first), command_source(4))
        .unwrap();
    builder.end_marked_content(command_source(5)).unwrap();
    let scene = builder.finish().unwrap();

    assert_eq!(scene.resources().len(), 2);
    assert_eq!(scene.resources()[0].object(), first);
    assert_eq!(scene.resources()[1].object(), second);
    assert_eq!(scene.commands()[0].properties().unwrap().value(), 0);
    assert_eq!(scene.commands()[2].properties().unwrap().value(), 1);
    assert_eq!(scene.commands()[4].properties().unwrap().value(), 0);
}

#[test]
fn scalar_normalizes_negative_zero_and_rejects_precision_and_overflow() {
    assert_eq!(SceneScalar::from_decimal("-0").unwrap(), SceneScalar::ZERO);
    assert_eq!(
        SceneScalar::from_decimal("-0.000000000").unwrap(),
        SceneScalar::ZERO
    );
    assert_eq!(
        SceneScalar::from_decimal("9223372036.854775807")
            .unwrap()
            .scaled(),
        i64::MAX
    );
    assert_eq!(
        SceneScalar::from_decimal("-9223372036.854775808")
            .unwrap()
            .scaled(),
        i64::MIN
    );
    assert_eq!(
        SceneScalar::from_decimal("0.0000000001")
            .unwrap_err()
            .code(),
        SceneErrorCode::ScalarPrecision
    );
    assert_eq!(
        SceneScalar::from_decimal("9223372036.854775808")
            .unwrap_err()
            .code(),
        SceneErrorCode::NumericOverflow
    );
    assert_eq!(
        SceneScalar::from_decimal("1e2").unwrap_err().code(),
        SceneErrorCode::InvalidScalar
    );
    assert_eq!(
        SceneScalar::from_scaled(1)
            .checked_mul(SceneScalar::from_decimal("0.5").unwrap())
            .unwrap()
            .scaled(),
        1
    );
    assert_eq!(
        SceneScalar::from_scaled(-1)
            .checked_mul(SceneScalar::from_decimal("0.5").unwrap())
            .unwrap()
            .scaled(),
        -1
    );
    assert_eq!(
        SceneScalar::from_scaled(i64::MAX)
            .checked_add(SceneScalar::from_scaled(1))
            .unwrap_err()
            .code(),
        SceneErrorCode::NumericOverflow
    );
    assert_eq!(
        SceneScalar::from_scaled(i64::MIN)
            .checked_sub(SceneScalar::from_scaled(1))
            .unwrap_err()
            .code(),
        SceneErrorCode::NumericOverflow
    );
}

#[test]
fn matrix_multiplication_is_checked_and_identity_stable() {
    let translation = Matrix::new([
        SceneScalar::ONE,
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        SceneScalar::ONE,
        scalar("12.5"),
        scalar("-3.25"),
    ]);
    assert_eq!(
        Matrix::IDENTITY.checked_multiply(translation).unwrap(),
        translation
    );
    assert_eq!(
        translation.checked_multiply(Matrix::IDENTITY).unwrap(),
        translation
    );

    let scale = Matrix::new([
        scalar("2"),
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar("3"),
        SceneScalar::ZERO,
        SceneScalar::ZERO,
    ]);
    assert_eq!(
        scale.checked_multiply(translation).unwrap().components(),
        [
            scalar("2"),
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            scalar("3"),
            scalar("25"),
            scalar("-9.75"),
        ]
    );
    let singular = Matrix::new([SceneScalar::ZERO; 6]);
    assert_eq!(
        singular.checked_multiply(Matrix::IDENTITY).unwrap(),
        singular
    );

    let huge = Matrix::new([SceneScalar::from_scaled(i64::MAX); 6]);
    assert_eq!(
        huge.checked_multiply(huge).unwrap_err().code(),
        SceneErrorCode::NumericOverflow
    );
}

#[test]
fn invalid_geometry_and_unbalanced_commands_do_not_publish_a_scene() {
    assert_eq!(
        SceneRect::new([
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            SceneScalar::ZERO,
            scalar("10"),
        ])
        .unwrap_err()
        .code(),
        SceneErrorCode::InvalidGeometry
    );
    assert_eq!(
        SceneRect::new([
            SceneScalar::from_scaled(i64::MIN),
            SceneScalar::ZERO,
            SceneScalar::from_scaled(i64::MAX),
            scalar("10"),
        ])
        .unwrap_err()
        .code(),
        SceneErrorCode::NumericOverflow
    );

    let mut builder = SceneBuilder::new(binding(4), geometry(), SceneLimits::default());
    assert_eq!(
        builder
            .end_marked_content(command_source(0))
            .unwrap_err()
            .code(),
        SceneErrorCode::InvalidCommandSequence
    );
    builder
        .begin_marked_content(b"Span", None, command_source(0))
        .unwrap();
    assert_eq!(
        builder.finish().unwrap_err().code(),
        SceneErrorCode::InvalidCommandSequence
    );
}

#[test]
fn command_depth_name_resource_and_retention_limits_are_prepublication_failures() {
    let mut command_limited = SceneBuilder::new(
        binding(5),
        geometry(),
        limits(|value| value.max_commands = 1),
    );
    command_limited
        .begin_marked_content(b"A", None, command_source(0))
        .unwrap();
    let error = command_limited
        .end_marked_content(command_source(1))
        .unwrap_err();
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::Commands);

    let mut depth_limited = SceneBuilder::new(
        binding(6),
        geometry(),
        limits(|value| value.max_marked_content_depth = 1),
    );
    depth_limited
        .begin_marked_content(b"A", None, command_source(0))
        .unwrap();
    let error = depth_limited
        .begin_marked_content(b"B", None, command_source(1))
        .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        SceneLimitKind::MarkedContentDepth
    );

    let mut name_limited = SceneBuilder::new(
        binding(7),
        geometry(),
        limits(|value| value.max_name_bytes = 1),
    );
    let error = name_limited
        .begin_marked_content(b"AB", None, command_source(0))
        .unwrap_err();
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::NameBytes);
    assert_eq!(error.command_index(), Some(0));

    let mut resource_limited = SceneBuilder::new(
        binding(8),
        geometry(),
        limits(|value| value.max_resources = 1),
    );
    resource_limited
        .begin_marked_content(
            b"A",
            Some(ObjectRef::new(10, 0).unwrap()),
            command_source(0),
        )
        .unwrap();
    resource_limited
        .end_marked_content(command_source(1))
        .unwrap();
    let error = resource_limited
        .begin_marked_content(
            b"B",
            Some(ObjectRef::new(11, 0).unwrap()),
            command_source(2),
        )
        .unwrap_err();
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::Resources);

    let mut retained_limited = SceneBuilder::new(
        binding(9),
        geometry(),
        limits(|value| value.max_retained_bytes = 1),
    );
    let error = retained_limited
        .begin_marked_content(b"A", None, command_source(0))
        .unwrap_err();
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::RetainedBytes);
    assert_eq!(error.command_index(), Some(0));
    assert!(retained_limited.finish().unwrap().commands().is_empty());
}

#[test]
fn canonical_output_has_an_independent_byte_budget() {
    let scene = SceneBuilder::new(
        binding(10),
        geometry(),
        limits(|value| value.max_canonical_bytes = 16),
    )
    .finish()
    .unwrap();
    let error = scene.canonical_json_bytes().unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        SceneLimitKind::CanonicalBytes
    );
}

#[test]
fn debug_output_redacts_names_and_runtime_source_digest() {
    let mut builder = SceneBuilder::new(binding(11), geometry(), SceneLimits::default());
    builder
        .begin_marked_content(b"private-tag", None, command_source(0))
        .unwrap();
    builder.end_marked_content(command_source(1)).unwrap();
    let debug = format!("{:?}", builder.finish().unwrap());
    assert!(!debug.contains("private-tag"));
    assert!(!debug.contains("0b0b0b0b"));
    assert!(debug.contains("[REDACTED]"));
}
