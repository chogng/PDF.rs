use pdf_rs_raster::reference::{
    NormalizedQ16, PremultipliedRgbaQ16, ReferenceBlendMode, ReferenceColorProfile,
    ReferenceDeviceColor, ReferenceOutputProfile, ReferenceSrgbQ16,
};
use pdf_rs_scene::{BlendMode, DeviceColor, Paint, SceneUnit};

const ONE: u32 = 1 << 16;

fn q16(bits: u32) -> NormalizedQ16 {
    NormalizedQ16::from_bits(bits).unwrap()
}

fn unit(bits: u16) -> SceneUnit {
    SceneUnit::from_u16(bits)
}

fn rgb(red: u16, green: u16, blue: u16) -> ReferenceDeviceColor {
    DeviceColor::Rgb {
        red: unit(red),
        green: unit(green),
        blue: unit(blue),
    }
}

fn cmyk(cyan: u16, magenta: u16, yellow: u16, black: u16) -> ReferenceDeviceColor {
    DeviceColor::Cmyk {
        cyan: unit(cyan),
        magenta: unit(magenta),
        yellow: unit(yellow),
        black: unit(black),
    }
}

fn raw_color(color: ReferenceSrgbQ16) -> [u32; 3] {
    [
        color.red().bits(),
        color.green().bits(),
        color.blue().bits(),
    ]
}

fn raw(pixel: PremultipliedRgbaQ16) -> [u32; 4] {
    [
        pixel.red().bits(),
        pixel.green().bits(),
        pixel.blue().bits(),
        pixel.alpha().bits(),
    ]
}

fn independently_round_q16(numerator: u64) -> u32 {
    u32::try_from((numerator + 32_768) / 65_536).unwrap()
}

fn independently_composite_channel(
    mode: ReferenceBlendMode,
    source: u32,
    source_alpha: u32,
    backdrop: u32,
    backdrop_alpha: u32,
) -> u32 {
    let one = u64::from(ONE);
    let source = u64::from(source);
    let source_alpha = u64::from(source_alpha);
    let backdrop = u64::from(backdrop);
    let backdrop_alpha = u64::from(backdrop_alpha);
    let numerator = match mode {
        ReferenceBlendMode::Normal => source * one + backdrop * (one - source_alpha),
        ReferenceBlendMode::Multiply => {
            backdrop * (one - source_alpha) + source * (one - backdrop_alpha) + source * backdrop
        }
        ReferenceBlendMode::Screen => source * one + backdrop * one - source * backdrop,
    };
    independently_round_q16(numerator)
}

fn independently_composite(
    mode: ReferenceBlendMode,
    source: [u32; 4],
    backdrop: [u32; 4],
) -> [u32; 4] {
    [
        independently_composite_channel(mode, source[0], source[3], backdrop[0], backdrop[3]),
        independently_composite_channel(mode, source[1], source[3], backdrop[1], backdrop[3]),
        independently_composite_channel(mode, source[2], source[3], backdrop[2], backdrop[3]),
        independently_round_q16(
            u64::from(source[3]) * u64::from(ONE)
                + u64::from(backdrop[3]) * u64::from(ONE - source[3]),
        ),
    ]
}

fn prepared(
    red: u16,
    green: u16,
    blue: u16,
    alpha: u16,
    mode: BlendMode,
) -> (PremultipliedRgbaQ16, ReferenceBlendMode) {
    ReferenceColorProfile::ReferenceColorV1.prepare_paint(Paint::new(
        rgb(red, green, blue),
        unit(alpha),
        mode,
    ))
}

#[test]
fn algorithm_identity_is_distinct_from_output_encoding_and_white_is_literal() {
    let profile = ReferenceColorProfile::ReferenceColorV1;
    let output = ReferenceOutputProfile::OpaqueSrgbStraightRgba8V1;
    assert_eq!(profile.label(), "reference-color-v1");
    assert_eq!(output.label(), "sRGB-reference-v1");
    assert_ne!(profile.label(), output.label());

    let (gray_white, gray_mode) = profile.prepare_paint(Paint::new(
        DeviceColor::Gray(SceneUnit::ONE),
        SceneUnit::ONE,
        BlendMode::Normal,
    ));
    assert_eq!(gray_mode, ReferenceBlendMode::Normal);
    assert_eq!(raw(gray_white), [65_536, 65_536, 65_536, 65_536]);
    assert_eq!(gray_white.to_straight_rgba8(), [255, 255, 255, 255]);

    let (cmyk_white, _) = profile.prepare_paint(Paint::new(
        cmyk(0, 0, 0, 0),
        SceneUnit::ONE,
        BlendMode::Normal,
    ));
    assert_eq!(raw(cmyk_white), [65_536, 65_536, 65_536, 65_536]);
    assert_eq!(cmyk_white.to_straight_rgba8(), [255, 255, 255, 255]);
}

#[test]
fn scene_unit_and_device_conversion_match_literal_independent_vectors() {
    let unit_vectors = [
        (0x0000, 0),
        (0x0001, 1),
        (0x1234, 4_660),
        (0x7fff, 32_767),
        (0x8000, 32_769),
        (0xabcd, 43_982),
        (0xfedc, 65_245),
        (0xfffe, 65_535),
        (0xffff, 65_536),
    ];
    for (scene_bits, expected) in unit_vectors {
        assert_eq!(NormalizedQ16::from(unit(scene_bits)).bits(), expected);
    }

    let profile = ReferenceColorProfile::ReferenceColorV1;
    let color_vectors = [
        (DeviceColor::Gray(unit(0x0000)), [0, 0, 0]),
        (DeviceColor::Gray(unit(0x8000)), [32_769, 32_769, 32_769]),
        (rgb(0x1234, 0x8000, 0xfedc), [4_660, 32_769, 65_245]),
        (
            cmyk(0x4000, 0x8000, 0xc000, 0x2000),
            [40_960, 24_575, 8_191],
        ),
        (
            cmyk(0x1234, 0x5678, 0x9abc, 0x1111),
            [56_507, 39_031, 21_554],
        ),
        (cmyk(0x9000, 0x1000, 0x7000, 0x8000), [0, 28_671, 4_095]),
        (cmyk(0xffff, 0, 0, 0), [0, 65_536, 65_536]),
        (cmyk(0, 0xffff, 0, 0), [65_536, 0, 65_536]),
        (cmyk(0, 0, 0xffff, 0), [65_536, 65_536, 0]),
        (cmyk(0, 0, 0, 0xffff), [0, 0, 0]),
    ];
    for (input, expected) in color_vectors {
        assert_eq!(raw_color(profile.convert(input)), expected);
    }
}

#[test]
fn scene_adapter_freezes_premultiplied_q16_ties_and_blend_mapping() {
    let profile = ReferenceColorProfile::ReferenceColorV1;
    let (source, mode) = profile.prepare_paint(Paint::new(
        rgb(0x1234, 0x8000, 0xfedc),
        unit(0xc000),
        BlendMode::Screen,
    ));
    assert_eq!(mode, ReferenceBlendMode::Screen);
    assert_eq!(raw(source), [3_495, 24_577, 48_935, 49_153]);

    let one = ReferenceSrgbQ16::gray(q16(1));
    assert_eq!(raw(one.with_constant_alpha(q16(32_767))), [0, 0, 0, 32_767]);
    assert_eq!(raw(one.with_constant_alpha(q16(32_768))), [1, 1, 1, 32_768]);
    assert_eq!(raw(one.with_constant_alpha(q16(32_769))), [1, 1, 1, 32_769]);
}

#[test]
fn premultiplied_invariants_constant_alpha_and_publication_boundaries_are_exact() {
    assert_eq!(NormalizedQ16::from_bits(0), Some(NormalizedQ16::ZERO));
    assert_eq!(NormalizedQ16::from_bits(ONE), Some(NormalizedQ16::ONE));
    assert_eq!(NormalizedQ16::from_bits(ONE + 1), None);
    assert!(
        PremultipliedRgbaQ16::new(q16(2), NormalizedQ16::ZERO, NormalizedQ16::ZERO, q16(1))
            .is_none()
    );
    assert_eq!(
        PremultipliedRgbaQ16::new(
            NormalizedQ16::ZERO,
            NormalizedQ16::ZERO,
            NormalizedQ16::ZERO,
            NormalizedQ16::ZERO,
        ),
        Some(PremultipliedRgbaQ16::TRANSPARENT)
    );
    assert_eq!(
        PremultipliedRgbaQ16::TRANSPARENT.to_straight_rgba8(),
        [0, 0, 0, 0]
    );

    let (opaque, _) = prepared(0x1111, 0x8080, 0xffff, 0xffff, BlendMode::Normal);
    assert_eq!(opaque.to_straight_rgba8(), [17, 128, 255, 255]);
    assert_eq!(opaque.apply_constant_alpha(NormalizedQ16::ONE), opaque);
    assert_eq!(
        opaque.apply_constant_alpha(NormalizedQ16::ZERO),
        PremultipliedRgbaQ16::TRANSPARENT
    );

    let half = opaque.apply_constant_alpha(q16(32_768));
    assert_eq!(half.alpha().bits(), 32_768);
    assert_eq!(half.to_straight_rgba8(), [17, 128, 255, 128]);
}

#[test]
fn unpremultiply_and_rgba8_half_up_publication_boundary_is_exact() {
    let alpha = q16(32_768);
    let below =
        PremultipliedRgbaQ16::new(q16(16_383), NormalizedQ16::ZERO, NormalizedQ16::ZERO, alpha)
            .unwrap();
    let tie =
        PremultipliedRgbaQ16::new(q16(16_384), NormalizedQ16::ZERO, NormalizedQ16::ZERO, alpha)
            .unwrap();
    let above =
        PremultipliedRgbaQ16::new(q16(16_385), NormalizedQ16::ZERO, NormalizedQ16::ZERO, alpha)
            .unwrap();

    assert_eq!(below.to_straight_rgba8(), [127, 0, 0, 128]);
    assert_eq!(tie.to_straight_rgba8(), [128, 0, 0, 128]);
    assert_eq!(above.to_straight_rgba8(), [128, 0, 0, 128]);
}

#[test]
fn normal_multiply_and_screen_layer_stacks_match_literal_vectors() {
    let layer_specs = [
        (0x1234, 0x8000, 0xfedc, 0xc000),
        (0xabcd, 0x4000, 0x2222, 0x9000),
        (0xffff, 0x0101, 0x7777, 0x6000),
    ];
    let expected = [
        (
            BlendMode::Normal,
            ReferenceBlendMode::Normal,
            [
                [3_495, 24_577, 48_935, 49_153],
                [26_270, 19_968, 26_323, 58_369],
                [40_995, 12_576, 27_921, 61_057],
            ],
        ),
        (
            BlendMode::Multiply,
            ReferenceBlendMode::Multiply,
            [
                [3_495, 24_577, 48_935, 49_153],
                [9_033, 16_512, 26_307, 58_369],
                [11_721, 10_355, 22_300, 61_057],
            ],
        ),
        (
            BlendMode::Screen,
            ReferenceBlendMode::Screen,
            [
                [3_495, 24_577, 48_935, 49_153],
                [26_917, 30_337, 50_180, 58_369],
                [41_399, 30_389, 52_867, 61_057],
            ],
        ),
    ];

    for (scene_mode, reference_mode, expected_steps) in expected {
        let mut backdrop = PremultipliedRgbaQ16::TRANSPARENT;
        for (index, (red, green, blue, alpha)) in layer_specs.into_iter().enumerate() {
            let (source, mapped_mode) = prepared(red, green, blue, alpha, scene_mode);
            assert_eq!(mapped_mode, reference_mode);
            backdrop = mapped_mode.source_over(source, backdrop);
            assert_eq!(raw(backdrop), expected_steps[index]);
        }
    }
}

#[test]
fn mixed_mode_layer_stack_matches_literal_vector() {
    let layers = [
        (0x1234, 0x8000, 0xfedc, 0xc000, BlendMode::Normal),
        (0xabcd, 0x4000, 0x2222, 0x9000, BlendMode::Multiply),
        (0xffff, 0x0101, 0x7777, 0x6000, BlendMode::Screen),
    ];
    let expected = [
        [3_495, 24_577, 48_935, 49_153],
        [9_033, 16_512, 26_307, 58_369],
        [30_222, 16_584, 33_172, 61_057],
    ];

    let mut backdrop = PremultipliedRgbaQ16::TRANSPARENT;
    for (index, (red, green, blue, alpha, mode)) in layers.into_iter().enumerate() {
        let (source, mode) = prepared(red, green, blue, alpha, mode);
        backdrop = mode.source_over(source, backdrop);
        assert_eq!(raw(backdrop), expected[index]);
    }
}

#[test]
fn independently_enumerated_layered_shapes_match_literal_pixels() {
    let (white, normal) = prepared(0xffff, 0xffff, 0xffff, 0xffff, BlendMode::Normal);
    let (red, red_mode) = prepared(0xffff, 0, 0, 0xffff, BlendMode::Normal);
    let (blue, multiply) = prepared(0, 0, 0xffff, 0xffff, BlendMode::Multiply);
    let (green, screen) = prepared(0, 0xffff, 0, 0x8000, BlendMode::Screen);
    assert_eq!(normal, ReferenceBlendMode::Normal);
    assert_eq!(red_mode, ReferenceBlendMode::Normal);
    assert_eq!(multiply, ReferenceBlendMode::Multiply);
    assert_eq!(screen, ReferenceBlendMode::Screen);

    let red_left_two_columns = [
        true, true, false, //
        true, true, false, //
        true, true, false,
    ];
    let blue_top_two_rows = [
        true, true, true, //
        true, true, true, //
        false, false, false,
    ];
    let green_anti_diagonal = [
        false, false, true, //
        false, true, false, //
        true, false, false,
    ];

    let mut pixels = [white; 9];
    for index in 0..pixels.len() {
        if red_left_two_columns[index] {
            pixels[index] = red_mode.source_over(red, pixels[index]);
        }
        if blue_top_two_rows[index] {
            pixels[index] = multiply.source_over(blue, pixels[index]);
        }
        if green_anti_diagonal[index] {
            pixels[index] = screen.source_over(green, pixels[index]);
        }
    }

    assert_eq!(
        pixels.map(PremultipliedRgbaQ16::to_straight_rgba8),
        [
            [0, 0, 0, 255],
            [0, 0, 0, 255],
            [0, 128, 255, 255],
            [0, 0, 0, 255],
            [0, 128, 0, 255],
            [0, 0, 255, 255],
            [255, 128, 0, 255],
            [255, 0, 0, 255],
            [255, 255, 255, 255],
        ]
    );
}

#[test]
fn every_blend_mode_matches_an_independent_boundary_grid() {
    let alpha_values = [0, 1, 32_767, 32_768, 32_769, 65_535, 65_536];
    let modes = [
        ReferenceBlendMode::Normal,
        ReferenceBlendMode::Multiply,
        ReferenceBlendMode::Screen,
    ];

    for source_alpha in alpha_values {
        for backdrop_alpha in alpha_values {
            let source_channels = [0, source_alpha / 2, source_alpha];
            let backdrop_channels = [0, backdrop_alpha / 2, backdrop_alpha];
            for source_channel in source_channels {
                for backdrop_channel in backdrop_channels {
                    let source = PremultipliedRgbaQ16::new(
                        q16(source_channel),
                        q16(source_channel),
                        q16(source_channel),
                        q16(source_alpha),
                    )
                    .unwrap();
                    let backdrop = PremultipliedRgbaQ16::new(
                        q16(backdrop_channel),
                        q16(backdrop_channel),
                        q16(backdrop_channel),
                        q16(backdrop_alpha),
                    )
                    .unwrap();
                    for mode in modes {
                        let actual = mode.source_over(source, backdrop);
                        let expected = independently_composite(
                            mode,
                            [source_channel, source_channel, source_channel, source_alpha],
                            [
                                backdrop_channel,
                                backdrop_channel,
                                backdrop_channel,
                                backdrop_alpha,
                            ],
                        );
                        assert_eq!(raw(actual), expected);
                        assert!(actual.red().bits() <= actual.alpha().bits());
                        assert!(actual.green().bits() <= actual.alpha().bits());
                        assert!(actual.blue().bits() <= actual.alpha().bits());
                    }
                }
            }
        }
    }
}

#[test]
fn conversion_and_compositing_metamorphic_relations_hold() {
    let profile = ReferenceColorProfile::ReferenceColorV1;
    for value in u16::MIN..=u16::MAX {
        let value = unit(value);
        assert_eq!(
            profile.convert(DeviceColor::Gray(value)),
            profile.convert(DeviceColor::Rgb {
                red: value,
                green: value,
                blue: value,
            })
        );
    }

    let (first, _) = prepared(0x1234, 0x8000, 0xfedc, 0xc000, BlendMode::Normal);
    let (second, _) = prepared(0xabcd, 0x4000, 0x2222, 0x9000, BlendMode::Normal);
    for mode in [ReferenceBlendMode::Multiply, ReferenceBlendMode::Screen] {
        assert_eq!(
            mode.source_over(first, second),
            mode.source_over(second, first)
        );
    }
    for mode in [
        ReferenceBlendMode::Normal,
        ReferenceBlendMode::Multiply,
        ReferenceBlendMode::Screen,
    ] {
        assert_eq!(
            mode.source_over(PremultipliedRgbaQ16::TRANSPARENT, first),
            first
        );
        assert_eq!(
            mode.source_over(first, PremultipliedRgbaQ16::TRANSPARENT),
            first
        );
    }

    let (opaque, _) = prepared(0x1111, 0x8080, 0xffff, 0xffff, BlendMode::Normal);
    assert_eq!(
        ReferenceBlendMode::Normal.source_over(opaque, first),
        opaque
    );

    let permuted_first =
        PremultipliedRgbaQ16::new(first.blue(), first.red(), first.green(), first.alpha()).unwrap();
    let permuted_second =
        PremultipliedRgbaQ16::new(second.blue(), second.red(), second.green(), second.alpha())
            .unwrap();
    for mode in [
        ReferenceBlendMode::Normal,
        ReferenceBlendMode::Multiply,
        ReferenceBlendMode::Screen,
    ] {
        let original = mode.source_over(first, second);
        let permuted = mode.source_over(permuted_first, permuted_second);
        assert_eq!(
            raw(permuted),
            [
                original.blue().bits(),
                original.red().bits(),
                original.green().bits(),
                original.alpha().bits(),
            ]
        );
    }
}
