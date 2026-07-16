use std::mem::{needs_drop, size_of};

use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_scene::{
    CommandSource, PageGeometry, PageRotation, Scene, SceneBinding, SceneBuilder, SceneDiffField,
    SceneDiffKind, SceneDiffLimitConfig, SceneDiffLimits, SceneDiffSection, SceneDifference,
    SceneErrorCode, SceneLimitKind, SceneLimits, SceneRect, SceneScalar, compare_scenes,
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

fn geometry(rotation: PageRotation) -> PageGeometry {
    let media = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar("100"),
        scalar("200"),
    ])
    .unwrap();
    PageGeometry::new(media, media, rotation)
}

fn binding(
    source_salt: u8,
    revision_startxref: u64,
    page_index: u32,
    page_object: u32,
) -> SceneBinding {
    SceneBinding::new(
        source(source_salt),
        revision_startxref,
        page_index,
        ObjectRef::new(page_object, 0).unwrap(),
    )
}

fn command_source(object: u32, operator_index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(object, 0).unwrap(),
        0,
        u64::from(operator_index) * 4,
        3,
        operator_index,
    )
    .unwrap()
}

fn empty_scene(source_salt: u8) -> Scene {
    SceneBuilder::new(
        binding(source_salt, 42, 0, 3),
        geometry(PageRotation::Degrees0),
        SceneLimits::default(),
    )
    .finish()
    .unwrap()
}

fn one_pair_scene(source_salt: u8, tag: &[u8], property: Option<u32>, source_object: u32) -> Scene {
    let mut builder = SceneBuilder::new(
        binding(source_salt, 42, 0, 3),
        geometry(PageRotation::Degrees0),
        SceneLimits::default(),
    );
    builder
        .begin_marked_content(
            tag,
            property.map(|number| ObjectRef::new(number, 0).unwrap()),
            command_source(source_object, 0),
        )
        .unwrap();
    builder
        .end_marked_content(command_source(source_object, 1))
        .unwrap();
    builder.finish().unwrap()
}

fn actual_section_scene() -> Scene {
    let mut builder = SceneBuilder::new(
        binding(99, 77, 7, 8),
        geometry(PageRotation::Degrees90),
        SceneLimits::default(),
    );
    builder
        .begin_marked_content(
            b"Artifact",
            Some(ObjectRef::new(10, 0).unwrap()),
            command_source(5, 0),
        )
        .unwrap();
    builder.end_marked_content(command_source(5, 1)).unwrap();
    builder
        .begin_marked_content(b"Extra", None, command_source(5, 2))
        .unwrap();
    builder.end_marked_content(command_source(5, 3)).unwrap();
    builder.finish().unwrap()
}

fn diff_limits(mut update: impl FnMut(&mut SceneDiffLimitConfig)) -> SceneDiffLimits {
    let mut config = SceneDiffLimitConfig::default();
    update(&mut config);
    SceneDiffLimits::validate(config).unwrap()
}

#[test]
fn runtime_source_identity_noise_is_ignored() {
    let expected = empty_scene(1);
    let actual = empty_scene(200);
    assert_ne!(expected.binding().source(), actual.binding().source());

    let difference = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();
    assert!(difference.is_exact());
    assert!(difference.differences().is_empty());
    assert_eq!(difference.stats().differences(), 0);
    assert_eq!(difference.stats().retained_bytes(), 0);
}

#[test]
fn semantic_sections_have_stable_changed_added_and_removed_order() {
    let expected = one_pair_scene(1, b"Span", None, 4);
    let actual = actual_section_scene();
    let difference = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();
    let observed = difference
        .differences()
        .iter()
        .copied()
        .map(|entry| (entry.section(), entry.field(), entry.kind(), entry.index()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed,
        vec![
            (
                SceneDiffSection::Binding,
                SceneDiffField::PageIndex,
                SceneDiffKind::Changed,
                None,
            ),
            (
                SceneDiffSection::Binding,
                SceneDiffField::PageObject,
                SceneDiffKind::Changed,
                None,
            ),
            (
                SceneDiffSection::Binding,
                SceneDiffField::RevisionStartxref,
                SceneDiffKind::Changed,
                None,
            ),
            (
                SceneDiffSection::Geometry,
                SceneDiffField::Rotation,
                SceneDiffKind::Changed,
                None,
            ),
            (
                SceneDiffSection::Features,
                SceneDiffField::Entry,
                SceneDiffKind::Added,
                Some(1),
            ),
            (
                SceneDiffSection::Resources,
                SceneDiffField::Entry,
                SceneDiffKind::Added,
                Some(0),
            ),
            (
                SceneDiffSection::Commands,
                SceneDiffField::Entry,
                SceneDiffKind::Changed,
                Some(0),
            ),
            (
                SceneDiffSection::Commands,
                SceneDiffField::Entry,
                SceneDiffKind::Added,
                Some(2),
            ),
            (
                SceneDiffSection::Commands,
                SceneDiffField::Entry,
                SceneDiffKind::Added,
                Some(3),
            ),
            (
                SceneDiffSection::CommandProvenance,
                SceneDiffField::Entry,
                SceneDiffKind::Changed,
                Some(0),
            ),
            (
                SceneDiffSection::CommandProvenance,
                SceneDiffField::Entry,
                SceneDiffKind::Changed,
                Some(1),
            ),
            (
                SceneDiffSection::CommandProvenance,
                SceneDiffField::Entry,
                SceneDiffKind::Added,
                Some(2),
            ),
            (
                SceneDiffSection::CommandProvenance,
                SceneDiffField::Entry,
                SceneDiffKind::Added,
                Some(3),
            ),
        ]
    );
    assert_eq!(difference.stats().differences(), 13);
    assert_eq!(difference.stats().changed(), 7);
    assert_eq!(difference.stats().added(), 6);
    assert_eq!(difference.stats().removed(), 0);

    let reverse = compare_scenes(&actual, &expected, SceneDiffLimits::default()).unwrap();
    assert_eq!(reverse.stats().differences(), 13);
    assert_eq!(reverse.stats().changed(), 7);
    assert_eq!(reverse.stats().added(), 0);
    assert_eq!(reverse.stats().removed(), 6);
    assert_eq!(reverse.differences()[4].kind(), SceneDiffKind::Removed);
    assert_eq!(
        reverse.differences()[4].section(),
        SceneDiffSection::Features
    );
}

#[test]
fn canonical_diff_json_has_an_exact_content_redacted_golden() {
    let expected = empty_scene(1);
    let actual = one_pair_scene(77, b"private-tag", None, 4);
    let difference = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();
    let canonical = difference.canonical_json_bytes().unwrap();
    assert_eq!(
        std::str::from_utf8(&canonical).unwrap(),
        "{\"differences\":[{\"field\":\"entry\",\"index\":0,\"kind\":\"added\",\"section\":\"features\"},{\"field\":\"entry\",\"index\":0,\"kind\":\"added\",\"section\":\"commands\"},{\"field\":\"entry\",\"index\":1,\"kind\":\"added\",\"section\":\"commands\"},{\"field\":\"entry\",\"index\":0,\"kind\":\"added\",\"section\":\"command-provenance\"},{\"field\":\"entry\",\"index\":1,\"kind\":\"added\",\"section\":\"command-provenance\"}],\"schema\":{\"major\":1,\"minor\":0,\"name\":\"scene-semantic-diff\"},\"summary\":{\"added\":5,\"changed\":0,\"removed\":0,\"total\":5}}"
    );
    assert!(
        !canonical
            .windows(b"private-tag".len())
            .any(|window| window == b"private-tag")
    );
    assert!(!canonical.windows(8).any(|window| window == b"4d4d4d4d"));
}

#[test]
fn one_less_than_each_diff_budget_is_a_structured_failure() {
    let expected = empty_scene(1);
    let actual = one_pair_scene(2, b"Span", None, 4);
    let complete = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();
    assert_eq!(complete.stats().differences(), 5);

    let error = compare_scenes(
        &expected,
        &actual,
        diff_limits(|value| value.max_differences = 4),
    )
    .unwrap_err();
    assert_eq!(error.code(), SceneErrorCode::ResourceLimit);
    let evidence = error.limit().unwrap();
    assert_eq!(evidence.kind(), SceneLimitKind::Differences);
    assert_eq!(evidence.limit(), 4);
    assert_eq!(evidence.consumed(), 4);
    assert_eq!(evidence.attempted(), 1);

    let retained_bytes = complete.stats().retained_bytes();
    assert!(retained_bytes > 1);
    let error = compare_scenes(
        &expected,
        &actual,
        diff_limits(|value| value.max_retained_bytes = retained_bytes - 1),
    )
    .unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        SceneLimitKind::DiffRetainedBytes
    );

    let canonical_bytes = u64::try_from(complete.canonical_json_bytes().unwrap().len()).unwrap();
    assert!(canonical_bytes > 1);
    let limited = compare_scenes(
        &expected,
        &actual,
        diff_limits(|value| value.max_canonical_bytes = canonical_bytes - 1),
    )
    .unwrap();
    let error = limited.canonical_json_bytes().unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        SceneLimitKind::DiffCanonicalBytes
    );
}

#[test]
fn diff_limits_reject_every_zero_dimension() {
    let config = SceneDiffLimitConfig {
        max_differences: 0,
        ..SceneDiffLimitConfig::default()
    };
    assert_eq!(
        SceneDiffLimits::validate(config).unwrap_err().code(),
        SceneErrorCode::InvalidLimits
    );

    let config = SceneDiffLimitConfig {
        max_retained_bytes: 0,
        ..SceneDiffLimitConfig::default()
    };
    assert_eq!(
        SceneDiffLimits::validate(config).unwrap_err().code(),
        SceneErrorCode::InvalidLimits
    );

    let config = SceneDiffLimitConfig {
        max_canonical_bytes: 0,
        ..SceneDiffLimitConfig::default()
    };
    assert_eq!(
        SceneDiffLimits::validate(config).unwrap_err().code(),
        SceneErrorCode::InvalidLimits
    );
}

#[test]
fn fixed_records_repeat_and_debug_without_content_or_profile_noise() {
    assert_eq!(size_of::<SceneDifference>(), 8);
    assert!(!needs_drop::<SceneDifference>());

    let first_expected = empty_scene(3);
    let first_actual = one_pair_scene(4, b"secret-name", None, 4);
    let second_expected = empty_scene(83);
    let second_actual = one_pair_scene(84, b"secret-name", None, 4);
    let first = compare_scenes(&first_expected, &first_actual, SceneDiffLimits::default()).unwrap();
    let second =
        compare_scenes(&second_expected, &second_actual, SceneDiffLimits::default()).unwrap();

    assert_eq!(first.differences(), second.differences());
    assert_eq!(first.stats(), second.stats());
    assert_eq!(
        first.canonical_json_bytes().unwrap(),
        second.canonical_json_bytes().unwrap()
    );
    let first_debug = format!("{first:?}");
    let second_debug = format!("{second:?}");
    assert_eq!(first_debug, second_debug);
    assert!(first_debug.contains("[REDACTED]"));
    assert!(!first_debug.contains("secret-name"));
    assert!(!first_debug.contains("03030303"));
}
