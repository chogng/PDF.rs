use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FUZZ_SEEDS: [&str; 4] = ["fill", "clip", "image", "glyph"];
const FUZZ_MANIFEST: &str = include_str!("../fuzz/Cargo.toml");
const FUZZ_TARGET: &str = include_str!("../fuzz/fuzz_targets/m4_fast_raster.rs");

#[test]
fn registered_m4_fast_raster_fuzz_target_is_bounded_and_differential() {
    for required in [
        "name = \"m4fastraster\"",
        "path = \"fuzz_targets/m4_fast_raster.rs\"",
    ] {
        assert!(
            FUZZ_MANIFEST.contains(required),
            "missing Fast raster fuzz manifest field: {required}"
        );
    }
    for required in [
        "fuzz_target!(|data: &[u8]|",
        "const MAX_FUZZ_INPUT: usize = 256;",
        "assert_eq!(fast.tiles().len(), 4);",
        "assert_eq!(compose(&fast), reference_pixels(&scene));",
    ] {
        assert!(
            FUZZ_TARGET.contains(required),
            "missing bounded differential invariant: {required}"
        );
    }

    let quality = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for (family, seed_name) in FUZZ_SEEDS.into_iter().enumerate() {
        let seed = fs::read(quality.join("fuzz/corpus/m4_fast_raster").join(seed_name))
            .expect("registered Fast raster seed remains readable");
        assert!(!seed.is_empty());
        assert_eq!(
            usize::from(seed[0] % 4),
            family,
            "each registered seed must enter its named Fast raster family"
        );
        assert!(seed.len() <= 256);
    }
}

#[test]
fn registered_m4_fast_raster_fuzz_target_builds_locked() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz/Cargo.toml");
    let status = Command::new("cargo")
        .arg("check")
        .arg("--locked")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--bin")
        .arg("m4fastraster")
        .status()
        .expect("cargo must launch for the registered Fast raster fuzz target");
    assert!(
        status.success(),
        "registered Fast raster fuzz target must compile from its lockfile"
    );
}

#[test]
fn registered_m4_fast_raster_fuzz_target_replays_fixed_seed() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fuzz");
    let corpus = TempDirectory::new("pdf-rs-m4-fast-raster-fuzz");
    let artifacts = corpus.path().join("artifacts");
    fs::create_dir(&artifacts).expect("temporary artifact directory must be created");
    for seed in FUZZ_SEEDS {
        fs::copy(
            package.join("corpus/m4_fast_raster").join(seed),
            corpus.path().join(seed),
        )
        .expect("registered seed must copy into the temporary replay corpus");
    }

    let artifact_prefix = format!("-artifact_prefix={}/", artifacts.display());
    let status = Command::new("cargo")
        .arg("fuzz")
        .arg("run")
        .arg("--fuzz-dir")
        .arg(&package)
        .arg("--sanitizer")
        .arg("none")
        .arg("m4fastraster")
        .arg(corpus.path())
        .arg("--")
        .arg("-seed=20260718")
        .arg("-runs=64")
        .arg("-max_len=256")
        .arg("-timeout=1")
        .arg("-rss_limit_mb=512")
        .arg(artifact_prefix)
        .status()
        .expect("cargo-fuzz must launch the registered Fast raster target");
    assert!(
        status.success(),
        "bounded fixed-seed Fast raster replay must pass"
    );
    assert_eq!(
        fs::read_dir(&artifacts)
            .expect("artifact directory remains readable")
            .count(),
        0,
        "successful replay must not produce a crash artifact"
    );
}

struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn new(prefix: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()));
        fs::create_dir(&path).expect("temporary corpus directory must be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("temporary corpus directory must be removable");
    }
}
