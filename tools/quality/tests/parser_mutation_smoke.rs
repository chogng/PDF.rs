use pdf_rs_benchmark::{
    BenchmarkReportLimits, decode_report as decode_benchmark_report, encode_report,
};
use pdf_rs_corpus::{CorpusManifestLimits, decode_manifest, encode_manifest};
use pdf_rs_digest::{hex_digest, sha256};
use pdf_rs_generate::{GenerateLimits, compile_dsl};

const PROFILE: &str = "m0.parser-mutation-smoke.v1";
const CODEC_INPUT_LIMIT: usize = 2_048;
const MUTANT_HARD_LIMIT: usize = 4_096;
const MUTATIONS_PER_TARGET: usize = 103;
const TARGET_COUNT: usize = 3;
const GLOBAL_MUTATION_LIMIT: usize = MUTATIONS_PER_TARGET * TARGET_COUNT;
const SYNTHETIC_SECRET: &str = "M0_SYNTHETIC_SECRET_DO_NOT_ECHO";

const GENERATOR_SEED: &[u8] =
    include_bytes!("../../../tests/cases/infrastructure/synthetic-failure-bundle-001/source.dsl");
const CORPUS_SEED: &[u8] = include_bytes!("../../../tests/corpus/manifests/t0-bootstrap-v1.toml");
const BENCHMARK_SEED: &[u8] =
    include_bytes!("../../../tests/performance/m0-synthetic-benchmark-replay-v1.toml");

const REPLACEMENTS: &[(u8, &str)] = &[
    (0x00, "nul"),
    (0x0a, "line-feed"),
    (0x0d, "carriage-return"),
    (0x22, "quote"),
    (0x5c, "backslash"),
    (0x80, "invalid-utf8-80"),
    (0xff, "invalid-utf8-ff"),
];

const INSERTIONS: &[(&[u8], &str)] = &[
    (b"\0", "nul"),
    (b"\xff", "invalid-utf8"),
    (b"\"\\\r", "quote-backslash-control"),
    (b"[]{}=,", "delimiters"),
    (b"18446744073709551616", "u64-overflow"),
    (SYNTHETIC_SECRET.as_bytes(), "synthetic-secret"),
];

#[derive(Clone, Eq, PartialEq)]
enum Observation {
    Accepted {
        source_sha256: [u8; 32],
        artifact_sha256: [u8; 32],
        artifact: Vec<u8>,
    },
    Rejected(ErrorFingerprint),
}

#[derive(Clone, Eq, PartialEq)]
struct ErrorFingerprint {
    code: String,
    category: String,
    recoverability: String,
    diagnostic_id: &'static str,
    primary_position: Option<usize>,
    secondary_position: Option<usize>,
    display: String,
    debug: String,
}

#[derive(Clone, Copy)]
struct Target {
    id: &'static str,
    seed: &'static [u8],
    expected_seed_sha256: &'static str,
    sensitive_marker: &'static str,
    expected_limit_code: &'static str,
    observe: fn(&[u8]) -> Observation,
}

#[test]
fn canonical_parsers_survive_the_fixed_deterministic_mutation_profile() {
    let targets = [
        Target {
            id: "generator-dsl",
            seed: GENERATOR_SEED,
            expected_seed_sha256: "70f51cde27704589f0d2018d7bc18fd3595c7a95246f1791b02076b211727e85",
            sensitive_marker: "q\\nQ\\n",
            expected_limit_code: "SourceLimit",
            observe: observe_generator,
        },
        Target {
            id: "corpus-manifest",
            seed: CORPUS_SEED,
            expected_seed_sha256: "4268cb945b6056d7732f22b0e90d9629f6d31ab2ba6f013e7011735989859d8e",
            sensitive_marker: "synthetic-failure-bundle-001/input.pdf",
            expected_limit_code: "ManifestLimit",
            observe: observe_corpus,
        },
        Target {
            id: "benchmark-report",
            seed: BENCHMARK_SEED,
            expected_seed_sha256: "2d66bab0542d92e443922d4a2d2ee72f382558d5c35153bc598370747d621527",
            sensitive_marker: "not-measured-synthetic",
            expected_limit_code: "ReportLimit",
            observe: observe_benchmark,
        },
    ];

    let mut global_count = 0usize;
    for target in targets {
        let count = exercise_target(target);
        global_count = global_count.checked_add(count).unwrap_or_else(|| {
            panic!("profile={PROFILE} invariant=global-mutation-count-overflow")
        });
        assert!(
            global_count <= GLOBAL_MUTATION_LIMIT,
            "profile={PROFILE} invariant=global-mutation-limit"
        );
    }
    assert!(
        global_count == GLOBAL_MUTATION_LIMIT,
        "profile={PROFILE} invariant=global-mutation-count"
    );
}

fn exercise_target(target: Target) -> usize {
    assert!(
        !target.seed.is_empty() && target.seed.len() <= CODEC_INPUT_LIMIT,
        "profile={PROFILE} parser={} invariant=seed-bound",
        target.id
    );
    let seed_hash = sha256(target.seed)
        .map(|digest| hex_digest(&digest))
        .unwrap_or_else(|_| panic!("profile={PROFILE} parser={} invariant=seed-hash", target.id));
    assert!(
        seed_hash == target.expected_seed_sha256,
        "profile={PROFILE} parser={} invariant=seed-identity",
        target.id
    );

    let first_seed = (target.observe)(target.seed);
    let second_seed = (target.observe)(target.seed);
    assert_observation_safe(&target, "canonical-seed", &first_seed);
    assert_observation_safe(&target, "canonical-seed", &second_seed);
    assert!(
        first_seed == second_seed,
        "profile={PROFILE} parser={} mutation=canonical-seed invariant=repeatability",
        target.id
    );
    assert!(
        matches!(first_seed, Observation::Accepted { .. }),
        "profile={PROFILE} parser={} mutation=canonical-seed invariant=acceptance",
        target.id
    );

    let anchors = anchors(target.seed.len());
    let mut mutation_count = 0usize;

    for &position in &anchors[..6] {
        let mutation_id = format!("truncate@{position}");
        let mut bytes = bounded_vec(position, &target, &mutation_id);
        bytes.extend_from_slice(&target.seed[..position]);
        run_mutation(&target, &mutation_id, bytes, &mut mutation_count);
    }

    for &position in &anchors[..6] {
        let mutation_id = format!("delete@{position}");
        let mut bytes = bounded_vec(target.seed.len() - 1, &target, &mutation_id);
        bytes.extend_from_slice(&target.seed[..position]);
        bytes.extend_from_slice(&target.seed[position + 1..]);
        run_mutation(&target, &mutation_id, bytes, &mut mutation_count);
    }

    for &position in &anchors[..6] {
        for &(replacement, name) in REPLACEMENTS {
            let mutation_id = format!("replace-{name}@{position}");
            let mut bytes = bounded_vec(target.seed.len(), &target, &mutation_id);
            bytes.extend_from_slice(target.seed);
            bytes[position] = replacement;
            run_mutation(&target, &mutation_id, bytes, &mut mutation_count);
        }
    }

    for &position in &anchors {
        for &(insertion, name) in INSERTIONS {
            let length = target
                .seed
                .len()
                .checked_add(insertion.len())
                .unwrap_or_else(|| {
                    panic!(
                        "profile={PROFILE} parser={} mutation=insert-{name}@{position} invariant=length-overflow",
                        target.id
                    )
                });
            let mutation_id = format!("insert-{name}@{position}");
            let mut bytes = bounded_vec(length, &target, &mutation_id);
            bytes.extend_from_slice(&target.seed[..position]);
            bytes.extend_from_slice(insertion);
            bytes.extend_from_slice(&target.seed[position..]);
            run_mutation(&target, &mutation_id, bytes, &mut mutation_count);
        }
    }

    let duplicate_starts = [
        0,
        1,
        target.seed.len() / 4,
        target.seed.len() / 2,
        target.seed.len().checked_mul(3).unwrap_or_else(|| {
            panic!(
                "profile={PROFILE} parser={} invariant=anchor-overflow",
                target.id
            )
        }) / 4,
        target.seed.len() - 8,
    ];
    assert!(
        duplicate_starts.windows(2).all(|pair| pair[0] < pair[1]),
        "profile={PROFILE} parser={} invariant=distinct-duplicate-windows",
        target.id
    );
    for start in duplicate_starts {
        let end = start.checked_add(8).unwrap_or_else(|| {
            panic!(
                "profile={PROFILE} parser={} mutation=duplicate-8@{start} invariant=length-overflow",
                target.id
            )
        });
        assert!(
            end <= target.seed.len(),
            "profile={PROFILE} parser={} mutation=duplicate-8@{start} invariant=window-bound",
            target.id
        );
        let length = target.seed.len().checked_add(8).unwrap_or_else(|| {
            panic!(
                "profile={PROFILE} parser={} mutation=duplicate-8@{start} invariant=length-overflow",
                target.id
            )
        });
        let mutation_id = format!("duplicate-8@{start}");
        let mut bytes = bounded_vec(length, &target, &mutation_id);
        bytes.extend_from_slice(&target.seed[..end]);
        bytes.extend_from_slice(&target.seed[start..end]);
        bytes.extend_from_slice(&target.seed[end..]);
        run_mutation(&target, &mutation_id, bytes, &mut mutation_count);
    }

    let mutation_id = "input-limit-plus-one";
    let mut oversized = bounded_vec(CODEC_INPUT_LIMIT + 1, &target, mutation_id);
    oversized.resize(CODEC_INPUT_LIMIT + 1, b'x');
    let limit_observation = run_mutation(&target, mutation_id, oversized, &mut mutation_count);
    assert_rejection_code(
        &target,
        mutation_id,
        &limit_observation,
        target.expected_limit_code,
    );

    assert!(
        mutation_count == MUTATIONS_PER_TARGET,
        "profile={PROFILE} parser={} invariant=mutation-count",
        target.id
    );
    mutation_count
}

#[test]
fn mutation_profile_remains_test_only_and_not_fuzz_evidence() {
    let feature_map = include_str!("../../../docs/traceability/feature-map.toml");
    for feature_id in [
        "quality.minimal-pdf-generator",
        "quality.corpus-manager",
        "quality.benchmark-harness",
    ] {
        let record = array_record(feature_map, "[[feature]]", feature_id);
        assert!(
            record.contains("tools/quality/tests/parser_mutation_smoke.rs"),
            "profile={PROFILE} feature={feature_id} invariant=test-trace"
        );
        assert!(
            record.lines().any(|line| line == "state = \"PLANNED\""),
            "profile={PROFILE} feature={feature_id} invariant=planned-state"
        );
        assert!(
            record.lines().any(|line| line == "fuzz_targets = []"),
            "profile={PROFILE} feature={feature_id} invariant=no-fuzz-target"
        );
    }

    let spec_map = include_str!("../../../docs/traceability/spec-map.toml");
    for requirement_id in [
        "RPE-ARCH-001/15.3/M0",
        "RPE-ARCH-001/12.6",
        "RPE-ARCH-001/12.19",
        "RPE-ARCH-001/12.21",
    ] {
        let record = array_record(spec_map, "[[requirement]]", requirement_id);
        assert!(
            record.contains("tools/quality/tests/parser_mutation_smoke.rs"),
            "profile={PROFILE} requirement={requirement_id} invariant=spec-test-trace"
        );
        assert!(
            record.lines().any(|line| line == "status = \"partial\""),
            "profile={PROFILE} requirement={requirement_id} invariant=partial-status"
        );
    }
    let milestone = array_record(spec_map, "[[requirement]]", "RPE-ARCH-001/15.3/M0");
    assert!(
        milestone.contains("registered coverage-guided and continuous fuzzing")
            && milestone.contains("remain open"),
        "profile={PROFILE} invariant=continuous-fuzz-open"
    );

    let registry = include_str!("../../../tests/fuzz/README.md");
    for boundary in [
        "No fuzz target is registered in M0",
        "no coverage guidance",
        "not continuous-fuzz evidence",
        "does not satisfy a release fuzz gate",
    ] {
        assert!(
            registry.contains(boundary),
            "profile={PROFILE} invariant=registry-boundary"
        );
    }
}

fn array_record<'a>(document: &'a str, header: &str, id: &str) -> &'a str {
    let expected = format!("id = \"{id}\"");
    document
        .split(header)
        .skip(1)
        .find(|record| record.lines().any(|line| line == expected))
        .unwrap_or_else(|| panic!("profile={PROFILE} invariant=missing-trace-record id={id}"))
}

fn anchors(length: usize) -> [usize; 7] {
    assert!(
        length >= 8,
        "profile={PROFILE} invariant=minimum-seed-length"
    );
    let three_quarters = length
        .checked_mul(3)
        .unwrap_or_else(|| panic!("profile={PROFILE} invariant=anchor-overflow"))
        / 4;
    let values = [
        0,
        1,
        length / 4,
        length / 2,
        three_quarters,
        length - 1,
        length,
    ];
    assert!(
        values.windows(2).all(|pair| pair[0] < pair[1]),
        "profile={PROFILE} invariant=distinct-anchors"
    );
    values
}

fn bounded_vec(length: usize, target: &Target, mutation_id: &str) -> Vec<u8> {
    assert!(
        length <= MUTANT_HARD_LIMIT,
        "profile={PROFILE} parser={} mutation={mutation_id} invariant=mutant-hard-limit",
        target.id
    );
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).unwrap_or_else(|_| {
        panic!(
            "profile={PROFILE} parser={} mutation={mutation_id} invariant=allocation",
            target.id
        )
    });
    bytes
}

fn run_mutation(
    target: &Target,
    mutation_id: &str,
    bytes: Vec<u8>,
    mutation_count: &mut usize,
) -> Observation {
    assert!(
        bytes.len() <= MUTANT_HARD_LIMIT,
        "profile={PROFILE} parser={} mutation={mutation_id} invariant=mutant-hard-limit",
        target.id
    );
    *mutation_count = mutation_count.checked_add(1).unwrap_or_else(|| {
        panic!(
            "profile={PROFILE} parser={} mutation={mutation_id} invariant=count-overflow",
            target.id
        )
    });
    assert!(
        *mutation_count <= MUTATIONS_PER_TARGET,
        "profile={PROFILE} parser={} mutation={mutation_id} invariant=target-mutation-limit",
        target.id
    );

    let first = (target.observe)(&bytes);
    let second = (target.observe)(&bytes);
    assert_observation_safe(target, mutation_id, &first);
    assert_observation_safe(target, mutation_id, &second);
    assert!(
        first == second,
        "profile={PROFILE} parser={} mutation={mutation_id} invariant=repeatability seed_sha256={}",
        target.id,
        target.expected_seed_sha256
    );
    first
}

fn assert_observation_safe(target: &Target, mutation_id: &str, observation: &Observation) {
    let Observation::Rejected(error) = observation else {
        return;
    };
    assert!(
        error.category != "Internal" && error.code != "HashFailed",
        "profile={PROFILE} parser={} mutation={mutation_id} invariant=non-internal-error seed_sha256={}",
        target.id,
        target.expected_seed_sha256
    );
    for marker in [SYNTHETIC_SECRET, target.sensitive_marker] {
        assert!(
            !error.display.contains(marker) && !error.debug.contains(marker),
            "profile={PROFILE} parser={} mutation={mutation_id} invariant=redacted-error seed_sha256={}",
            target.id,
            target.expected_seed_sha256
        );
    }
}

fn assert_rejection_code(
    target: &Target,
    mutation_id: &str,
    observation: &Observation,
    expected_code: &str,
) {
    let Observation::Rejected(error) = observation else {
        panic!(
            "profile={PROFILE} parser={} mutation={mutation_id} invariant=limit-rejection",
            target.id
        );
    };
    assert!(
        error.code == expected_code,
        "profile={PROFILE} parser={} mutation={mutation_id} invariant=limit-code",
        target.id
    );
}

fn observe_generator(input: &[u8]) -> Observation {
    let limits = GenerateLimits::new(CODEC_INPUT_LIMIT, 4_096, 64, 1_048_576, 2_097_152)
        .unwrap_or_else(|_| panic!("profile={PROFILE} parser=generator-dsl invariant=limits"));
    match compile_dsl(input, limits) {
        Ok(generated) => {
            let source_sha256 = sha256(input).unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=generator-dsl invariant=source-hash")
            });
            let artifact_sha256 = sha256(generated.bytes()).unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=generator-dsl invariant=artifact-hash")
            });
            assert!(
                source_sha256 == generated.source_sha256()
                    && artifact_sha256 == generated.output_sha256(),
                "profile={PROFILE} parser=generator-dsl invariant=reported-hashes"
            );
            Observation::Accepted {
                source_sha256,
                artifact_sha256,
                artifact: generated.into_bytes(),
            }
        }
        Err(error) => Observation::Rejected(ErrorFingerprint {
            code: format!("{:?}", error.code),
            category: format!("{:?}", error.category),
            recoverability: format!("{:?}", error.recoverability),
            diagnostic_id: error.diagnostic_id,
            primary_position: error.byte_offset,
            secondary_position: None,
            display: error.to_string(),
            debug: format!("{error:?}"),
        }),
    }
}

fn observe_corpus(input: &[u8]) -> Observation {
    let limits =
        CorpusManifestLimits::new(CODEC_INPUT_LIMIT, 128, 16, 32, 512, 1_048_576, 1_048_576)
            .unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=corpus-manifest invariant=limits")
            });
    match decode_manifest(input, limits) {
        Ok(manifest) => {
            let canonical = encode_manifest(&manifest, limits).unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=corpus-manifest invariant=canonical-encode")
            });
            assert!(
                canonical == input,
                "profile={PROFILE} parser=corpus-manifest invariant=canonical-bytes"
            );
            let artifact_sha256 = sha256(&canonical).unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=corpus-manifest invariant=artifact-hash")
            });
            assert!(
                artifact_sha256 == manifest.source_sha256(),
                "profile={PROFILE} parser=corpus-manifest invariant=reported-hash"
            );
            Observation::Accepted {
                source_sha256: manifest.source_sha256(),
                artifact_sha256,
                artifact: canonical,
            }
        }
        Err(error) => Observation::Rejected(ErrorFingerprint {
            code: format!("{:?}", error.code),
            category: format!("{:?}", error.category),
            recoverability: format!("{:?}", error.recoverability),
            diagnostic_id: error.diagnostic_id,
            primary_position: error.line,
            secondary_position: error.entry_index,
            display: error.to_string(),
            debug: format!("{error:?}"),
        }),
    }
}

fn observe_benchmark(input: &[u8]) -> Observation {
    let limits = BenchmarkReportLimits::new(CODEC_INPUT_LIMIT, 128, 32, 128, 512)
        .unwrap_or_else(|_| panic!("profile={PROFILE} parser=benchmark-report invariant=limits"));
    match decode_benchmark_report(input, limits) {
        Ok(report) => {
            let canonical = encode_report(&report, limits).unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=benchmark-report invariant=canonical-encode")
            });
            assert!(
                canonical == input,
                "profile={PROFILE} parser=benchmark-report invariant=canonical-bytes"
            );
            let artifact_sha256 = sha256(&canonical).unwrap_or_else(|_| {
                panic!("profile={PROFILE} parser=benchmark-report invariant=artifact-hash")
            });
            assert!(
                artifact_sha256 == report.source_sha256(),
                "profile={PROFILE} parser=benchmark-report invariant=reported-hash"
            );
            Observation::Accepted {
                source_sha256: report.source_sha256(),
                artifact_sha256,
                artifact: canonical,
            }
        }
        Err(error) => Observation::Rejected(ErrorFingerprint {
            code: format!("{:?}", error.code),
            category: format!("{:?}", error.category),
            recoverability: format!("{:?}", error.recoverability),
            diagnostic_id: error.diagnostic_id,
            primary_position: error.line,
            secondary_position: None,
            display: error.to_string(),
            debug: format!("{error:?}"),
        }),
    }
}
