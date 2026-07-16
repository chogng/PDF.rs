use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn product_filters_only_depend_on_bytes_and_syntax_and_have_no_platform_io() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml"))
        .expect("crate manifest must be readable during repository tests");
    let dependency_body = manifest
        .split_once("[dependencies]")
        .expect("crate manifest declares an explicit dependency table")
        .1
        .split("\n[")
        .next()
        .expect("dependency table body is present")
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        dependency_body,
        [
            r#"pdf-rs-bytes = { path = "../bytes" }"#,
            r#"pdf-rs-syntax = { path = "../syntax" }"#,
        ],
        "core/filters may depend only on lower-level bytes and syntax primitives"
    );
    assert!(
        !manifest.contains("[dev-dependencies]"),
        "core/filters must not introduce development dependencies"
    );

    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources);
    sources.sort();
    assert!(!sources.is_empty(), "filter source selection is non-empty");
    let forbidden = [
        "std::fs",
        "std::net",
        "async fn",
        "tokio",
        "async_std",
        "reqwest",
        "hyper",
        "pdfium",
        "mupdf",
        "pdf.js",
        "flate2",
        "weezl",
        "openssl",
    ];
    for path in sources {
        let source = fs::read_to_string(&path).expect("selected Rust source must be readable");
        let lowercase = source.to_ascii_lowercase();
        for token in forbidden {
            assert!(
                !lowercase.contains(token),
                "forbidden product filter token {token:?} in {}",
                path.display()
            );
        }
    }
}

#[test]
fn decoded_products_are_sealed_non_clone_and_identity_is_not_a_public_filter() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = fs::read_to_string(crate_root.join("src/model.rs")).unwrap();
    assert!(!model.contains("impl Clone for DecodeAttestation"));
    assert!(!model.contains("impl Clone for DecodedStream"));
    assert!(!model.contains("#[derive(Clone)]\npub struct DecodeAttestation"));
    assert!(!model.contains("#[derive(Clone)]\npub struct DecodedStream"));

    let public_filter = model
        .split_once("pub enum StreamFilter")
        .unwrap()
        .1
        .split_once("\n}\n\nimpl StreamFilter")
        .unwrap()
        .0;
    assert!(
        !public_filter.contains("Identity"),
        "identity must remain an internal empty-plan path"
    );
    for declaration in [
        "pub struct DecodeAttestation {",
        "pub struct DecodedStream {",
    ] {
        let body = model
            .split_once(declaration)
            .unwrap()
            .1
            .split_once("\n}")
            .unwrap()
            .0;
        assert!(
            !body
                .lines()
                .any(|line| line.trim_start().starts_with("pub ")),
            "sealed decoded-product fields must not be public"
        );
    }
    for required in [
        "pub fn retained_heap_bytes(&self)",
        "pub fn retained_heap_upper_bound(max_filters: u16)",
        "self.filters.capacity()",
        "self.stages.capacity()",
        "plan_retained_heap_bytes",
        "DecodeLimitKind::FilterPlanBytes",
    ] {
        assert!(
            model.contains(required),
            "sealed plan retention evidence must use {required:?}"
        );
    }
    assert!(!model.contains("fn retained_heap_bound("));
}

#[test]
fn direct_dictionary_canonicalization_is_filters_owned_bounded_and_cancellable() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = fs::read_to_string(crate_root.join("src/model.rs")).unwrap();
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md")).unwrap();

    for required in [
        "pub fn from_pdf_dictionary<C: DecodeCancellation + ?Sized>(",
        "unique_metadata_value",
        "canonical_filter_shape",
        "canonical_pdf_filter",
        "canonical_parameter_stage",
        "Self::allocate(filter_count, limits.max_filters())",
        "limits.max_filters()",
        "plan.validate_retained_heap_limit(limits.max_filters())",
        "check_metadata_cancelled(cancellation)?",
        "dictionary.entries().is_empty()",
        "DecodeErrorCode::InvalidDecodeParameters",
    ] {
        assert!(
            model.contains(required),
            "direct dictionary canonicalizer must retain {required:?}"
        );
    }
    for required in [
        "filters-owned shared direct-metadata canonicalizer",
        "Metadata keys must be unique",
        "equal-length direct array",
        "max_filters` limit before",
        "throughout every outer",
        "creates no additional",
        "one private full-name",
        "object-stream layer now uses the direct dictionary canonicalizer",
        "single-filter scalar/array shape policy is",
        "explicit compatibility decision",
    ] {
        assert!(
            provenance.contains(required),
            "dictionary canonicalization provenance must state {required:?}"
        );
    }
}

#[test]
fn predictor_support_is_parameter_attested_without_claiming_lzw_or_integration() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model = fs::read_to_string(crate_root.join("src/model.rs")).unwrap();
    let predictor = fs::read_to_string(crate_root.join("src/predictor.rs")).unwrap();
    let provenance = fs::read_to_string(crate_root.join("PROVENANCE.md")).unwrap();

    for required in [
        "pub struct FilterStage",
        "pub enum FilterDecodeParameters",
        "pub struct PredictorParameters",
        "predictor: 1",
        "colors: 1",
        "bits_per_component: 8",
        "columns: 1",
    ] {
        assert!(model.contains(required), "model must contain {required:?}");
    }
    for required in [
        "fn decode_tiff",
        "fn decode_png",
        "fn paeth",
        "output.charge_algorithm",
        "InvalidPredictorData",
    ] {
        assert!(
            predictor.contains(required),
            "predictor implementation must contain {required:?}"
        );
    }
    for required in [
        "TIFF Predictor 2",
        "Every PNG predictor value at or above 10",
        "LZW, DCT, CCITT",
        "indirect `/DecodeParms`",
        "source-driven decode scheduling",
        "read-only inspected",
        "No PDFium code or table was copied",
        "O4, unregistered, non-gating observer",
        "derived from ISO 32000-1:2008 section 7.4.4.4",
        "every packed input/output bit read",
        "do not use bulk fuel charges",
    ] {
        assert!(
            provenance.contains(required),
            "predictor provenance must contain {required:?}"
        );
    }
}

#[test]
fn traceability_registers_basic_filters_without_claiming_stream_integration() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = crate_root
        .parent()
        .and_then(Path::parent)
        .expect("core/filters has a repository root two levels above it");
    let feature_map =
        fs::read_to_string(repository_root.join("docs/traceability/feature-map.toml")).unwrap();
    let spec_map =
        fs::read_to_string(repository_root.join("docs/traceability/spec-map.toml")).unwrap();
    assert_eq!(top_level_version(&feature_map), Some("0.64.0"));
    assert_eq!(top_level_version(&spec_map), Some("0.64.0"));

    let feature = record_with_id(&feature_map, "feature", "core.stream-filter-decode")
        .expect("basic stream-filter feature must exist");
    for required in [
        "state = \"PLANNED\"",
        "profile = \"m1.basic-stream-filters.v1\"",
        "RPE-ARCH-001/5.6",
        "modules = [\"core/filters\"]",
        "core/filters::decode_behavior",
        "core/filters::predictor_behavior",
        "core/filters::repository_policy",
        "fuzz_targets = []",
        "benchmarks = []",
    ] {
        assert!(
            feature.contains(required),
            "feature must contain {required:?}"
        );
    }

    let requirement = record_with_id(&spec_map, "requirement", "RPE-ARCH-001/5.6")
        .expect("stream-filter requirement must exist");
    for required in [
        "sealed evidence",
        "ASCIIHexDecode",
        "ASCII85Decode",
        "RunLengthDecode",
        "FlateDecode",
        "stored, fixed-Huffman, and dynamic-Huffman",
        "charges block, Huffman, input, output, and cancellation work",
        "TIFF Predictor 2 over packed 1-, 2-, 4-, 8-, and 16-bit samples",
        "every PNG predictor value at or above 10 through row tags 0 through 4",
        "The opt-in `OpenSourceXrefStreamJob` and `OpenSourceRevisionChainJob` paths now construct",
        "`parse_filtered_object_stream` now re-canonicalizes the exact source dictionary",
        "LZW, non-predictor decode parameters",
        "source-acquired document owner now binds exact filtered object-stream payload acquisition",
        "Every affected feature remains PLANNED",
        "does not claim general stream support or M1 exit",
        "status = \"partial\"",
    ] {
        assert!(
            requirement.contains(required),
            "requirement must contain {required:?}"
        );
    }
}

fn collect_rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(directory).expect("source directory must be readable");
    for entry in entries {
        let entry = entry.expect("source directory entry must be readable");
        let path = entry.path();
        let file_type = entry
            .file_type()
            .expect("source file type must be readable");
        if file_type.is_dir() {
            collect_rust_sources(&path, output);
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "rs") {
            output.push(path);
        }
    }
}

fn top_level_version(input: &str) -> Option<&str> {
    input.lines().find_map(|line| {
        line.trim()
            .strip_prefix("version = \"")
            .and_then(|value| value.strip_suffix('"'))
    })
}

fn record_with_id<'a>(input: &'a str, table: &str, identity: &str) -> Option<&'a str> {
    let marker = format!("[[{table}]]");
    input.split(&marker).skip(1).find(|record| {
        record
            .lines()
            .any(|line| line.trim() == format!("id = \"{identity}\""))
    })
}
