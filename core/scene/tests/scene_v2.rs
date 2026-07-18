use pdf_rs_bytes::{SourceIdentity, SourceRevision, SourceStableId};
use pdf_rs_scene::{
    BlendMode, CapabilityContext, CapabilityStatus, CommandSource, DashPattern, DeviceColor,
    FillRule, GlyphOutline, GlyphUse, GraphicsCapability, GraphicsCommand, GraphicsResource,
    GraphicsResourceSource, GraphicsSceneBuilder, GraphicsSceneLimitConfig, GraphicsSceneLimits,
    ImageColorSpace, ImageResource, LineCap, LineJoin, LineStyle, Matrix, PageGeometry,
    PageRotation, Paint, PathResource, PathSegment, SceneBinding, SceneBounds, SceneDiffKind,
    SceneDiffLimits, SceneDiffSection, SceneErrorCode, SceneLimitKind, ScenePoint, SceneRect,
    SceneScalar, SceneUnit, compare_scenes,
};
use pdf_rs_syntax::ObjectRef;

fn scalar(value: &str) -> SceneScalar {
    SceneScalar::from_decimal(value).unwrap()
}

fn point(x: &str, y: &str) -> ScenePoint {
    ScenePoint::new(scalar(x), scalar(y))
}

fn binding(salt: u8) -> SceneBinding {
    SceneBinding::new(
        SourceIdentity::new(
            SourceStableId::new([salt; 32]),
            SourceRevision::new(u64::from(salt) + 1),
        ),
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
        u64::from(index) * 8,
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

fn bounds(max_x: &str) -> SceneBounds {
    SceneBounds::finite(point("1", "2"), point(max_x, "7")).unwrap()
}

fn black() -> Paint {
    Paint::new(
        DeviceColor::Gray(SceneUnit::ZERO),
        SceneUnit::ONE,
        BlendMode::Normal,
    )
}

fn glyph_outline() -> GlyphOutline {
    GlyphOutline::new(
        GraphicsResourceSource::new(ObjectRef::new(9, 0).unwrap(), 42, 11),
        7,
        1_000,
        path(),
    )
    .unwrap()
}

fn repeated_glyph_scene(
    max_retained_bytes: u64,
) -> Result<pdf_rs_scene::Scene, pdf_rs_scene::SceneError> {
    let limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_glyphs: 3,
        max_retained_bytes,
        ..GraphicsSceneLimitConfig::default()
    })?;
    let mut builder = GraphicsSceneBuilder::new_v2(binding(7), geometry(), limits);
    builder.draw_glyph_run(
        vec![
            GlyphUse::new(glyph_outline(), Matrix::IDENTITY, 65),
            GlyphUse::new(glyph_outline(), Matrix::IDENTITY, 65),
            GlyphUse::new(glyph_outline(), Matrix::IDENTITY, 65),
        ],
        black(),
        SceneBounds::Page,
        source(0),
    )?;
    builder.finish()
}

fn builder() -> GraphicsSceneBuilder {
    GraphicsSceneBuilder::new_v2(binding(1), geometry(), GraphicsSceneLimits::default())
}

#[test]
fn v1_empty_canonical_bytes_remain_frozen() {
    let scene = pdf_rs_scene::SceneBuilder::new(
        binding(1),
        geometry(),
        pdf_rs_scene::SceneLimits::default(),
    )
    .finish()
    .unwrap();
    assert!(scene.graphics().is_none());
    assert_eq!(
        std::str::from_utf8(&scene.canonical_json_bytes().unwrap()).unwrap(),
        "{\"binding\":{\"page_index\":0,\"page_object\":{\"generation\":0,\"number\":3},\"revision_startxref\":42},\"commands\":[],\"features\":{\"decision\":\"supported\",\"tags\":[]},\"geometry\":{\"crop_box\":[0,0,100000000000,200000000000],\"media_box\":[0,0,100000000000,200000000000],\"rotation\":0},\"provenance\":[],\"resources\":[],\"schema\":{\"major\":1,\"minor\":0}}"
    );
}

#[test]
fn v2_fill_has_exact_canonical_bytes_and_first_use_requirements() {
    let mut builder = builder();
    builder
        .append_fill(
            path(),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(0),
        )
        .unwrap();
    let scene = builder.finish().unwrap();
    assert_eq!(scene.version().major(), 2);
    assert_eq!(scene.version().minor(), 0);
    assert!(scene.commands().is_empty());
    assert!(scene.resources().is_empty());
    let graphics = scene.graphics().unwrap();
    assert_eq!(graphics.commands().len(), 1);
    assert_eq!(graphics.resources().len(), 1);
    assert_eq!(graphics.requirements().len(), 2);
    assert!(graphics.is_supported());

    assert_eq!(
        std::str::from_utf8(&scene.canonical_json_bytes().unwrap()).unwrap(),
        "{\"binding\":{\"page_index\":0,\"page_object\":{\"generation\":0,\"number\":3},\"revision_startxref\":42},\"commands\":[{\"bounds\":[1000000000,2000000000,5000000000,7000000000],\"command\":{\"kind\":\"fill\",\"paint\":{\"alpha\":65535,\"blend_mode\":\"normal\",\"color\":{\"components\":[0],\"space\":\"device-gray\"}},\"path\":0,\"rule\":\"nonzero\",\"transform\":[1000000000,0,0,1000000000,0,0]},\"source\":{\"decoded_length\":2,\"decoded_start\":0,\"object\":{\"generation\":0,\"number\":4},\"operator_index\":0,\"stream_index\":0}}],\"geometry\":{\"crop_box\":[0,0,100000000000,200000000000],\"media_box\":[0,0,100000000000,200000000000],\"rotation\":0},\"requirements\":[{\"capability\":\"device-color\",\"context\":{\"kind\":\"command\",\"value\":0},\"dependencies\":[],\"id\":0,\"parameter\":1,\"status\":\"supported\"},{\"capability\":\"path-fill\",\"context\":{\"kind\":\"command\",\"value\":0},\"dependencies\":[],\"id\":1,\"parameter\":0,\"status\":\"supported\"}],\"resources\":[{\"id\":0,\"resource\":{\"kind\":\"path\",\"segments\":[{\"kind\":\"move-to\",\"point\":[1000000000,2000000000]},{\"kind\":\"line-to\",\"point\":[5000000000,2000000000]},{\"kind\":\"line-to\",\"point\":[5000000000,7000000000]},{\"kind\":\"close-path\"}]}}],\"schema\":{\"major\":2,\"minor\":0}}"
    );
}

#[test]
fn every_graphics_family_is_serialized_and_resources_follow_first_command_use() {
    let mut builder = builder();
    let dash = DashPattern::new(vec![scalar("3"), scalar("1")], scalar("2")).unwrap();
    let style = LineStyle::new(
        scalar("2"),
        LineCap::Round,
        LineJoin::Bevel,
        scalar("4"),
        dash,
        Matrix::IDENTITY,
    )
    .unwrap();
    let red = Paint::new(
        DeviceColor::Rgb {
            red: SceneUnit::ONE,
            green: SceneUnit::ZERO,
            blue: SceneUnit::ZERO,
        },
        SceneUnit::from_u16(32_768),
        BlendMode::Multiply,
    );
    builder.append_save(SceneBounds::Empty, source(0)).unwrap();
    builder
        .append_clip(
            path(),
            FillRule::EvenOdd,
            Matrix::IDENTITY,
            bounds("5"),
            source(1),
        )
        .unwrap();
    builder
        .append_fill_stroke(
            path(),
            FillRule::EvenOdd,
            red,
            black(),
            style,
            Matrix::IDENTITY,
            SceneBounds::Page,
            source(2),
        )
        .unwrap();

    let resource_source = GraphicsResourceSource::new(ObjectRef::new(9, 0).unwrap(), 42, 11);
    let image = ImageResource::new(
        resource_source,
        1,
        1,
        ImageColorSpace::DeviceRgb,
        8,
        false,
        vec![1, 2, 3],
    )
    .unwrap();
    builder
        .draw_image(
            image,
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Screen,
            SceneBounds::Page,
            source(3),
        )
        .unwrap();

    let outline = GlyphOutline::new(resource_source, 7, 1_000, path()).unwrap();
    builder
        .draw_glyph_run(
            vec![GlyphUse::new(outline, Matrix::IDENTITY, 65)],
            black(),
            SceneBounds::Page,
            source(4),
        )
        .unwrap();
    builder
        .begin_group(
            SceneUnit::from_u16(50_000),
            BlendMode::Normal,
            SceneBounds::Page,
            source(5),
        )
        .unwrap();
    builder.end_group(SceneBounds::Page, source(6)).unwrap();
    builder
        .append_restore(SceneBounds::Empty, source(7))
        .unwrap();
    let scene = builder.finish().unwrap();
    let graphics = scene.graphics().unwrap();

    assert_eq!(graphics.resources().len(), 3);
    assert!(matches!(
        graphics.resources()[0].resource(),
        GraphicsResource::Path(_)
    ));
    assert!(matches!(
        graphics.resources()[1].resource(),
        GraphicsResource::Image(_)
    ));
    assert!(matches!(
        graphics.resources()[2].resource(),
        GraphicsResource::GlyphOutline(_)
    ));
    assert!(matches!(
        graphics.commands()[2].command(),
        GraphicsCommand::FillStroke { path, .. } if path.value() == 0
    ));

    let canonical = String::from_utf8(scene.canonical_json_bytes().unwrap()).unwrap();
    for required in [
        "\"kind\":\"save\"",
        "\"kind\":\"clip\"",
        "\"kind\":\"fill-stroke\"",
        "\"kind\":\"draw-image\"",
        "\"decoded_hex\":\"010203\"",
        "\"kind\":\"draw-glyph-run\"",
        "\"kind\":\"glyph-outline\"",
        "\"kind\":\"begin-isolated-group\"",
        "\"kind\":\"end-isolated-group\"",
        "\"kind\":\"restore\"",
        "\"blend_mode\":\"multiply\"",
        "\"blend_mode\":\"screen\"",
        "\"cap\":\"round\"",
        "\"join\":\"bevel\"",
        "\"dash\":{\"array\":[3000000000,1000000000],\"phase\":2000000000}",
    ] {
        assert!(canonical.contains(required), "missing {required}");
    }
}

#[test]
fn compatible_scene_import_replays_commands_and_reinterns_resources() {
    let mut child = builder();
    child
        .append_fill(
            path(),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(10),
        )
        .unwrap();
    child
        .draw_image(
            ImageResource::new(
                GraphicsResourceSource::new(ObjectRef::new(12, 0).unwrap(), 42, 7),
                1,
                1,
                ImageColorSpace::DeviceRgb,
                8,
                false,
                vec![8, 9, 10],
            )
            .unwrap(),
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(11),
        )
        .unwrap();
    let child = child.finish().unwrap();

    let mut parent = builder();
    parent
        .append_fill(
            path(),
            FillRule::EvenOdd,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(0),
        )
        .unwrap();
    parent.append_scene(&child).unwrap();
    let imported = parent.finish().unwrap();
    let graphics = imported.graphics().unwrap();

    assert_eq!(graphics.commands().len(), 3);
    assert_eq!(graphics.resources().len(), 2);
    assert_eq!(graphics.commands()[1].source(), source(10));
    assert_eq!(graphics.commands()[2].source(), source(11));
    assert!(matches!(
        graphics.commands()[1].command(),
        GraphicsCommand::Fill { path, .. } if path.value() == 0
    ));
    assert!(matches!(
        graphics.commands()[2].command(),
        GraphicsCommand::DrawImage { image, .. } if image.value() == 1
    ));

    let mut mismatched =
        GraphicsSceneBuilder::new_v2(binding(2), geometry(), GraphicsSceneLimits::default());
    assert_eq!(
        mismatched.append_scene(&child).unwrap_err().code(),
        SceneErrorCode::InvalidCommandSequence
    );
}

#[test]
fn failed_append_does_not_consume_a_resource_identifier_or_command_slot() {
    let limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_resources: 1,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let mut builder = GraphicsSceneBuilder::new_v2(binding(2), geometry(), limits);
    builder
        .append_fill(
            path(),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(0),
        )
        .unwrap();

    let image = ImageResource::new(
        GraphicsResourceSource::new(ObjectRef::new(8, 0).unwrap(), 42, 0),
        1,
        1,
        ImageColorSpace::DeviceGray,
        8,
        false,
        vec![0],
    )
    .unwrap();
    let error = builder
        .draw_image(
            image,
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(1),
        )
        .unwrap_err();
    assert_eq!(error.code(), SceneErrorCode::ResourceLimit);

    builder
        .append_fill(
            path(),
            FillRule::EvenOdd,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(2),
        )
        .unwrap();
    let scene = builder.finish().unwrap();
    let graphics = scene.graphics().unwrap();
    assert_eq!(graphics.resources().len(), 1);
    assert_eq!(graphics.resources()[0].id().value(), 0);
    assert_eq!(graphics.commands().len(), 2);
}

#[test]
fn explicit_capability_dependencies_are_backward_only_unique_and_affect_support() {
    let mut primary = builder();
    let color = primary
        .add_requirement(
            GraphicsCapability::DeviceColor,
            3,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    let blend = primary
        .add_requirement(
            GraphicsCapability::Blend,
            1,
            CapabilityContext::Scene,
            vec![color],
            CapabilityStatus::Unsupported,
        )
        .unwrap();
    assert_eq!(blend.value(), 1);
    let repeated = primary
        .add_requirement(
            GraphicsCapability::Blend,
            1,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    assert_eq!(repeated.value(), 2);
    let soft_mask = primary
        .add_requirement(
            GraphicsCapability::SoftMask,
            0,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Unsupported,
        )
        .unwrap();
    assert_eq!(soft_mask.value(), 3);
    for invalid_dependencies in [vec![blend, color], vec![color, color]] {
        assert_eq!(
            primary
                .add_requirement(
                    GraphicsCapability::PathFill,
                    0,
                    CapabilityContext::Scene,
                    invalid_dependencies,
                    CapabilityStatus::Supported,
                )
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidCommandSequence
        );
    }

    let mut other = builder();
    let foreign_zero = other
        .add_requirement(
            GraphicsCapability::Clip,
            0,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )
        .unwrap();
    let mut empty = builder();
    assert_eq!(
        empty
            .add_requirement(
                GraphicsCapability::PathFill,
                0,
                CapabilityContext::Scene,
                vec![foreign_zero],
                CapabilityStatus::Supported,
            )
            .unwrap_err()
            .code(),
        SceneErrorCode::InvalidCommandSequence
    );

    let scene = primary.finish().unwrap();
    let graphics = scene.graphics().unwrap();
    assert!(!graphics.is_supported());
    assert_eq!(graphics.requirements()[1].dependencies(), &[color]);
    let canonical = String::from_utf8(scene.canonical_json_bytes().unwrap()).unwrap();
    assert!(canonical.contains("\"capability\":\"soft-mask\""));
}

#[test]
fn graphics_diff_separates_commands_bounds_resources_and_capabilities_with_schema_two() {
    let mut expected = builder();
    expected
        .append_fill(
            path(),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(0),
        )
        .unwrap();
    let expected = expected.finish().unwrap();

    let mut actual = builder();
    actual
        .append_fill(
            PathResource::new(vec![
                PathSegment::MoveTo(point("1", "2")),
                PathSegment::LineTo(point("6", "2")),
            ])
            .unwrap(),
            FillRule::EvenOdd,
            Paint::new(
                DeviceColor::Gray(SceneUnit::ONE),
                SceneUnit::ONE,
                BlendMode::Normal,
            ),
            Matrix::IDENTITY,
            bounds("6"),
            source(0),
        )
        .unwrap();
    actual
        .add_requirement(
            GraphicsCapability::ConstantAlpha,
            10,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Unsupported,
        )
        .unwrap();
    let actual = actual.finish().unwrap();

    let diff = compare_scenes(&expected, &actual, SceneDiffLimits::default()).unwrap();
    for section in [
        SceneDiffSection::GraphicsCommands,
        SceneDiffSection::GraphicsBounds,
        SceneDiffSection::GraphicsResources,
        SceneDiffSection::GraphicsCapabilities,
    ] {
        assert!(
            diff.differences()
                .iter()
                .any(|difference| difference.section() == section),
            "missing {section:?}"
        );
    }
    assert!(diff.differences().iter().any(|difference| {
        difference.section() == SceneDiffSection::GraphicsCapabilities
            && difference.kind() == SceneDiffKind::Added
    }));
    let canonical = String::from_utf8(diff.canonical_json_bytes().unwrap()).unwrap();
    assert!(canonical.contains("\"schema\":{\"major\":2,\"minor\":0"));
    assert!(canonical.contains("\"section\":\"graphics-commands\""));
    assert!(canonical.contains("\"section\":\"graphics-bounds\""));
    assert!(canonical.contains("\"section\":\"graphics-resources\""));
    assert!(canonical.contains("\"section\":\"graphics-capabilities\""));
}

#[test]
fn balance_and_every_zero_graphics_limit_reject_without_publication() {
    let mut unbalanced = builder();
    unbalanced
        .append_save(SceneBounds::Empty, source(0))
        .unwrap();
    assert_eq!(
        unbalanced.finish().unwrap_err().code(),
        SceneErrorCode::InvalidCommandSequence
    );

    let mutations: &[fn(&mut GraphicsSceneLimitConfig)] = &[
        |value| value.max_commands = 0,
        |value| value.max_resources = 0,
        |value| value.max_requirements = 0,
        |value| value.max_dependencies = 0,
        |value| value.max_path_segments = 0,
        |value| value.max_image_bytes = 0,
        |value| value.max_glyphs = 0,
        |value| value.max_state_depth = 0,
        |value| value.max_group_depth = 0,
        |value| value.max_retained_bytes = 0,
        |value| value.max_resource_index_work = 0,
        |value| value.max_canonical_bytes = 0,
    ];
    for mutate in mutations {
        let mut config = GraphicsSceneLimitConfig::default();
        mutate(&mut config);
        assert_eq!(
            GraphicsSceneLimits::validate(config).unwrap_err().code(),
            SceneErrorCode::InvalidLimits
        );
    }
}

#[test]
fn resource_payload_comparisons_have_exact_and_one_less_work_boundaries() {
    let exact_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_resource_index_work: 5,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let mut exact = GraphicsSceneBuilder::new_v2(binding(3), geometry(), exact_limits);
    for (index, rule) in [FillRule::Nonzero, FillRule::EvenOdd]
        .into_iter()
        .enumerate()
    {
        exact
            .append_fill(
                path(),
                rule,
                black(),
                Matrix::IDENTITY,
                bounds("5"),
                source(u32::try_from(index).unwrap()),
            )
            .unwrap();
    }
    assert_eq!(exact.resource_index_work(), 5);
    let exact = exact.finish().unwrap();
    assert_eq!(exact.graphics().unwrap().stats().resource_index_work(), 5);
    assert_eq!(exact.stats().resource_index_work(), 5);

    let one_less_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_resource_index_work: 4,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let mut one_less = GraphicsSceneBuilder::new_v2(binding(3), geometry(), one_less_limits);
    one_less
        .append_fill(
            path(),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(0),
        )
        .unwrap();
    let error = one_less
        .append_fill(
            path(),
            FillRule::EvenOdd,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(1),
        )
        .unwrap_err();
    assert_eq!(error.code(), SceneErrorCode::ResourceLimit);
    assert_eq!(
        error.limit().unwrap().kind(),
        SceneLimitKind::ResourceIndexWork
    );
    assert_eq!(error.limit().unwrap().attempted(), 5);
    assert_eq!(one_less.resource_index_work(), 0);
}

#[test]
fn retained_capacity_includes_nested_payloads_and_compacts_caller_slack() {
    let mut oversized = Vec::with_capacity(1_024);
    oversized.extend(path().segments().iter().copied());
    let oversized = PathResource::new(oversized).unwrap();

    let render = |path: PathResource, limits: GraphicsSceneLimits| {
        let mut builder = GraphicsSceneBuilder::new_v2(binding(4), geometry(), limits);
        builder
            .append_fill(
                path,
                FillRule::Nonzero,
                black(),
                Matrix::IDENTITY,
                bounds("5"),
                source(0),
            )
            .unwrap();
        builder.finish().unwrap()
    };
    let compact = render(path(), GraphicsSceneLimits::default());
    let oversized = render(oversized, GraphicsSceneLimits::default());
    assert_eq!(
        compact.graphics().unwrap().stats().retained_bytes(),
        oversized.graphics().unwrap().stats().retained_bytes()
    );

    let build_dependencies = |limits: GraphicsSceneLimits| {
        let mut builder = GraphicsSceneBuilder::new_v2(binding(5), geometry(), limits);
        let first = builder.add_requirement(
            GraphicsCapability::DeviceColor,
            1,
            CapabilityContext::Scene,
            Vec::new(),
            CapabilityStatus::Supported,
        )?;
        builder.add_requirement(
            GraphicsCapability::Blend,
            1,
            CapabilityContext::Scene,
            vec![first],
            CapabilityStatus::Supported,
        )?;
        builder.finish()
    };
    let complete = build_dependencies(GraphicsSceneLimits::default()).unwrap();
    let retained = complete.graphics().unwrap().stats().retained_bytes();
    let exact = build_dependencies(
        GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
            max_retained_bytes: retained,
            ..GraphicsSceneLimitConfig::default()
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(exact.graphics().unwrap().stats().retained_bytes(), retained);
    let error = build_dependencies(
        GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
            max_retained_bytes: retained - 1,
            ..GraphicsSceneLimitConfig::default()
        })
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code(), SceneErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::RetainedBytes);
}

#[test]
fn geometric_growth_falls_back_to_exact_capacity_at_transaction_live_boundary() {
    let accepts = |max_retained_bytes| {
        let limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
            max_retained_bytes,
            ..GraphicsSceneLimitConfig::default()
        })
        .unwrap();
        let mut candidate = GraphicsSceneBuilder::new_v2(binding(6), geometry(), limits);
        (0..5).all(|index| {
            candidate
                .append_save(SceneBounds::Empty, source(index))
                .is_ok()
        })
    };
    let mut lower = 1_u64;
    let mut upper = GraphicsSceneLimitConfig::default().max_retained_bytes;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        if accepts(middle) {
            upper = middle;
        } else {
            lower = middle + 1;
        }
    }
    let exact_retained = lower;

    let exact_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_retained_bytes: exact_retained,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let mut exact = GraphicsSceneBuilder::new_v2(binding(6), geometry(), exact_limits);
    for index in 0..5 {
        exact
            .append_save(SceneBounds::Empty, source(index))
            .unwrap();
    }
    assert!(exact.retained_bytes().unwrap() <= exact_retained);

    let one_less_limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_retained_bytes: exact_retained - 1,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let mut one_less = GraphicsSceneBuilder::new_v2(binding(6), geometry(), one_less_limits);
    for index in 0..4 {
        one_less
            .append_save(SceneBounds::Empty, source(index))
            .unwrap();
    }
    let error = one_less
        .append_save(SceneBounds::Empty, source(4))
        .unwrap_err();
    assert_eq!(error.code(), SceneErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::RetainedBytes);
}

#[test]
fn glyph_transaction_retained_boundary_is_exact_atomic_and_interns_repeated_outlines() {
    let mut lower = 1_u64;
    let mut upper = GraphicsSceneLimitConfig::default().max_retained_bytes;
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        if repeated_glyph_scene(middle).is_ok() {
            upper = middle;
        } else {
            lower = middle + 1;
        }
    }
    let minimum = lower;
    let exact = repeated_glyph_scene(minimum).unwrap();
    let graphics = exact.graphics().unwrap();
    assert_eq!(graphics.commands().len(), 1);
    assert_eq!(graphics.resources().len(), 1);
    let GraphicsCommand::DrawGlyphRun(run) = graphics.commands()[0].command() else {
        panic!("expected glyph run")
    };
    assert_eq!(run.glyphs().len(), 3);
    assert!(graphics.stats().retained_bytes() <= minimum);

    let limits = GraphicsSceneLimits::validate(GraphicsSceneLimitConfig {
        max_glyphs: 3,
        max_retained_bytes: minimum - 1,
        ..GraphicsSceneLimitConfig::default()
    })
    .unwrap();
    let mut failed = GraphicsSceneBuilder::new_v2(binding(7), geometry(), limits);
    let outline = glyph_outline();
    let error = failed
        .draw_glyph_run(
            vec![
                GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65),
                GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65),
                GlyphUse::new(outline, Matrix::IDENTITY, 65),
            ],
            black(),
            SceneBounds::Page,
            source(0),
        )
        .unwrap_err();
    assert_eq!(error.code(), SceneErrorCode::ResourceLimit);
    assert_eq!(error.limit().unwrap().kind(), SceneLimitKind::RetainedBytes);
    let failed_scene = failed.finish().unwrap();
    let failed_graphics = failed_scene.graphics().unwrap();
    assert!(failed_graphics.commands().is_empty());
    assert!(failed_graphics.resources().is_empty());
    assert!(failed_graphics.requirements().is_empty());
    assert_eq!(failed_graphics.stats().retained_bytes(), 0);

    let mut retry = GraphicsSceneBuilder::new_v2(binding(7), geometry(), limits);
    let outline = glyph_outline();
    retry
        .draw_glyph_run(
            vec![
                GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65),
                GlyphUse::new(outline.clone(), Matrix::IDENTITY, 65),
                GlyphUse::new(outline, Matrix::IDENTITY, 65),
            ],
            black(),
            SceneBounds::Page,
            source(0),
        )
        .unwrap_err();
    retry
        .draw_glyph_run(
            vec![GlyphUse::new(glyph_outline(), Matrix::IDENTITY, 65)],
            black(),
            SceneBounds::Page,
            source(1),
        )
        .unwrap();
    let retried = retry.finish().unwrap();
    let retried = retried.graphics().unwrap();
    assert_eq!(retried.commands().len(), 1);
    assert_eq!(retried.resources().len(), 1);
    let GraphicsCommand::DrawGlyphRun(run) = retried.commands()[0].command() else {
        panic!("expected glyph run")
    };
    assert_eq!(run.glyphs().len(), 1);
}

#[test]
fn visible_bounds_must_be_proven_conservative_or_use_page_fallback() {
    let mut builder = builder();
    let too_small = SceneBounds::finite(point("1", "2"), point("4", "7")).unwrap();
    for invalid in [SceneBounds::Empty, too_small] {
        assert_eq!(
            builder
                .append_fill(
                    path(),
                    FillRule::Nonzero,
                    black(),
                    Matrix::IDENTITY,
                    invalid,
                    source(0),
                )
                .unwrap_err()
                .code(),
            SceneErrorCode::InvalidGeometry
        );
    }

    let style = LineStyle::new(
        SceneScalar::ONE,
        LineCap::Butt,
        LineJoin::Miter,
        scalar("10"),
        DashPattern::new(Vec::new(), SceneScalar::ZERO).unwrap(),
        Matrix::IDENTITY,
    )
    .unwrap();
    assert_eq!(
        builder
            .append_stroke(
                path(),
                black(),
                style,
                Matrix::IDENTITY,
                bounds("5"),
                source(0),
            )
            .unwrap_err()
            .code(),
        SceneErrorCode::InvalidGeometry
    );
    builder
        .append_fill(
            path(),
            FillRule::Nonzero,
            black(),
            Matrix::IDENTITY,
            bounds("5"),
            source(0),
        )
        .unwrap();
}

#[test]
fn source_identity_rejects_conflicting_decoded_payloads() {
    let source_key = GraphicsResourceSource::new(ObjectRef::new(12, 0).unwrap(), 42, 7);
    let image = |sample| {
        ImageResource::new(
            source_key,
            1,
            1,
            ImageColorSpace::DeviceGray,
            8,
            false,
            vec![sample],
        )
        .unwrap()
    };
    let mut builder = builder();
    builder
        .draw_image(
            image(0),
            Matrix::IDENTITY,
            SceneUnit::ONE,
            BlendMode::Normal,
            SceneBounds::Page,
            source(0),
        )
        .unwrap();
    assert_eq!(
        builder
            .draw_image(
                image(1),
                Matrix::IDENTITY,
                SceneUnit::ONE,
                BlendMode::Normal,
                SceneBounds::Page,
                source(1),
            )
            .unwrap_err()
            .code(),
        SceneErrorCode::InvalidCommandSequence
    );
    let scene = builder.finish().unwrap();
    assert_eq!(scene.graphics().unwrap().resources().len(), 1);
    assert_eq!(scene.graphics().unwrap().commands().len(), 1);
}

#[test]
fn post_close_segments_require_explicit_normalized_subpath_restart() {
    let start = PathSegment::MoveTo(point("0", "0"));
    let line = PathSegment::LineTo(point("1", "0"));
    let closed = vec![start, line, PathSegment::ClosePath];
    for trailing in [
        PathSegment::LineTo(point("2", "0")),
        PathSegment::CubicTo {
            control_1: point("1", "1"),
            control_2: point("2", "1"),
            end: point("2", "0"),
        },
        PathSegment::ClosePath,
    ] {
        let mut invalid = closed.clone();
        invalid.push(trailing);
        assert_eq!(
            PathResource::new(invalid).unwrap_err().code(),
            SceneErrorCode::InvalidCommandSequence
        );
    }
    let mut normalized = closed;
    normalized.extend([
        PathSegment::MoveTo(point("0", "0")),
        PathSegment::LineTo(point("2", "0")),
    ]);
    PathResource::new(normalized).unwrap();
}
