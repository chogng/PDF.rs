use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_scene::{
    BlendMode, CommandSource, DashPattern, DeviceColor, GraphicsSceneBuilder, LineCap, LineJoin,
    LineStyle, Matrix, PageGeometry, PageRotation, Paint, PathResource, PathSegment, SceneBinding,
    SceneBounds, SceneDiffKind, SceneDiffLimitConfig, SceneDiffLimits, SceneDiffSection,
    SceneLimitKind, ScenePoint, SceneRect, SceneScalar, SceneUnit, compare_scenes,
};
use pdf_rs_syntax::ObjectRef;

fn scalar(value: &str) -> SceneScalar {
    SceneScalar::from_decimal(value).unwrap()
}

fn point(x: &str, y: &str) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn binding() -> SceneBinding {
    SceneBinding::new(
        SourceIdentity::new(SourceStableId::new([7; 32]), SourceRevision::new(1)),
        42,
        0,
        ObjectRef::new(3, 0).unwrap(),
    )
}

fn geometry() -> PageGeometry {
    let page = SceneRect::new([
        SceneScalar::ZERO,
        SceneScalar::ZERO,
        scalar("100"),
        scalar("200"),
    ])
    .unwrap();
    PageGeometry::new(page, page, PageRotation::Degrees0)
}

fn source(index: u32) -> CommandSource {
    CommandSource::new(
        ObjectRef::new(4, 0).unwrap(),
        0,
        u64::from(index) * 4,
        2,
        index,
    )
    .unwrap()
}

fn path() -> PathResource {
    PathResource::new(vec![
        PathSegment::MoveTo(point("1", "2")),
        PathSegment::LineTo(point("5", "2")),
        PathSegment::LineTo(point("5", "7")),
        PathSegment::ClosePath,
    ])
    .unwrap()
}

fn bounds() -> SceneBounds {
    SceneBounds::finite(point("1", "2"), point("5", "7")).unwrap()
}

fn paint() -> Paint {
    Paint::new(
        DeviceColor::Gray(SceneUnit::ZERO),
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn diff_limits(max_compare_work: u64) -> SceneDiffLimits {
    SceneDiffLimits::validate(SceneDiffLimitConfig {
        max_compare_work,
        ..SceneDiffLimitConfig::default()
    })
    .unwrap()
}

fn fill_scene(source_index: u32) -> pdf_rs_scene::Scene {
    let mut builder = GraphicsSceneBuilder::new_v2(binding(), geometry(), Default::default());
    builder
        .append_fill(
            path(),
            pdf_rs_scene::FillRule::Nonzero,
            paint(),
            Matrix::IDENTITY,
            bounds(),
            source(source_index),
        )
        .unwrap();
    builder.finish().unwrap()
}

fn stroke_scene(second_dash: &str) -> pdf_rs_scene::Scene {
    let mut builder = GraphicsSceneBuilder::new_v2(binding(), geometry(), Default::default());
    let dash = DashPattern::new(vec![scalar("1"), scalar(second_dash)], SceneScalar::ZERO).unwrap();
    let style = LineStyle::new(
        scalar("1"),
        LineCap::Butt,
        LineJoin::Miter,
        scalar("10"),
        dash,
        Matrix::IDENTITY,
    )
    .unwrap();
    builder
        .append_stroke(
            path(),
            paint(),
            style,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    builder.finish().unwrap()
}

#[test]
fn graphics_command_provenance_is_an_independent_section() {
    let expected = fill_scene(0);
    let actual = fill_scene(1);
    let diff = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();

    assert_eq!(diff.differences().len(), 1);
    let difference = diff.differences()[0];
    assert_eq!(
        difference.section(),
        SceneDiffSection::GraphicsCommandProvenance
    );
    assert_eq!(difference.kind(), SceneDiffKind::Changed);
    assert_eq!(difference.index(), Some(0));
    assert!(
        String::from_utf8(diff.canonical_json_bytes().unwrap())
            .unwrap()
            .contains("\"section\":\"graphics-command-provenance\"")
    );
}

#[test]
fn deep_graphics_payloads_are_precharged_and_exact_budget_repeats() {
    let expected = stroke_scene("2");
    let actual = stroke_scene("3");

    let error = compare_scenes(&expected, &actual, diff_limits(11)).unwrap_err();
    let evidence = error.limit().unwrap();
    assert_eq!(evidence.kind(), SceneLimitKind::DiffCompareWork);
    assert_eq!(evidence.consumed(), 9);
    assert_eq!(evidence.attempted(), 3);

    let error = compare_scenes(&expected, &actual, diff_limits(18)).unwrap_err();
    let evidence = error.limit().unwrap();
    assert_eq!(evidence.kind(), SceneLimitKind::DiffCompareWork);
    assert_eq!(evidence.consumed(), 14);
    assert_eq!(evidence.attempted(), 5);

    let complete = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();
    let exact_work = complete.stats().compare_work();
    assert!(exact_work > 18);
    let exact = compare_scenes(&expected, &actual, diff_limits(exact_work)).unwrap();
    assert_eq!(exact.stats().compare_work(), exact_work);
    let error = compare_scenes(&expected, &actual, diff_limits(exact_work - 1)).unwrap_err();
    assert_eq!(
        error.limit().unwrap().kind(),
        SceneLimitKind::DiffCompareWork
    );
}
