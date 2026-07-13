use std::fs;
use std::path::{Path, PathBuf};

const PRODUCT_ROOTS: &[&str] = &["core", "runtime", "platform"];
const FORBIDDEN_ENGINES: &[&str] = &[
    "pdfium", "pdf.js", "pdfjs", "mupdf", "poppler", "hayro", "vello",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PurityViolation {
    pub code: &'static str,
    pub manifest: PathBuf,
    pub token: String,
}

pub fn check_product_manifests(repository: &Path) -> Result<usize, Vec<PurityViolation>> {
    let mut manifests = Vec::new();
    let workspace_manifest = repository.join("Cargo.toml");
    if workspace_manifest.is_file() {
        manifests.push(workspace_manifest);
    }
    for root in PRODUCT_ROOTS {
        let path = repository.join(root);
        if path.is_dir() && collect_manifests(&path, &mut manifests).is_err() {
            return Err(vec![PurityViolation {
                code: "RPE-PURITY-0001",
                manifest: path,
                token: "unreadable-product-tree".into(),
            }]);
        }
    }
    manifests.sort();

    let mut violations = Vec::new();
    for manifest in &manifests {
        let input = match fs::read_to_string(manifest) {
            Ok(input) => input,
            Err(_) => {
                violations.push(PurityViolation {
                    code: "RPE-PURITY-0001",
                    manifest: manifest.clone(),
                    token: "unreadable-manifest".into(),
                });
                continue;
            }
        };
        for line in input.lines() {
            let line = line
                .split('#')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            for forbidden in FORBIDDEN_ENGINES {
                if line.contains(forbidden) {
                    violations.push(PurityViolation {
                        code: "RPE-PURITY-0002",
                        manifest: manifest.clone(),
                        token: (*forbidden).into(),
                    });
                }
            }
            if line.contains("path") && (line.contains("tools/") || line.contains("tools\\")) {
                violations.push(PurityViolation {
                    code: "RPE-PURITY-0003",
                    manifest: manifest.clone(),
                    token: "product-to-tools-path".into(),
                });
            }
        }
    }

    if violations.is_empty() {
        Ok(manifests.len())
    } else {
        Err(violations)
    }
}

fn collect_manifests(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_manifests(&path, output)?;
        } else if file_type.is_file() && entry.file_name() == "Cargo.toml" {
            output.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn ignores_tool_only_baseline_and_accepts_native_product_manifest() {
        let root = temp_dir("isolated");
        write_manifest(
            &root.join("core/bytes/Cargo.toml"),
            "[package]\nname = \"pdf-rs-bytes\"\n",
        );
        write_manifest(
            &root.join("tools/baseline/Cargo.toml"),
            "[package]\nname = \"pdfium-wrapper\"\n",
        );
        assert_eq!(check_product_manifests(&root), Ok(1));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_external_engine_and_reverse_tool_dependencies() {
        let root = temp_dir("forbidden");
        write_manifest(
            &root.join("runtime/engine/Cargo.toml"),
            "[dependencies]\npdfium = \"1\"\ncompare = { path = \"../../tools/compare\" }\n",
        );
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(
            violations
                .iter()
                .any(|value| value.code == "RPE-PURITY-0002")
        );
        assert!(
            violations
                .iter()
                .any(|value| value.code == "RPE-PURITY-0003")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_forbidden_workspace_dependencies() {
        let root = temp_dir("workspace-forbidden");
        write_manifest(
            &root.join("Cargo.toml"),
            "[workspace.dependencies]\nrender = { package = \"pdfium\", path = \"../pdfium\" }\n",
        );
        write_manifest(
            &root.join("core/bytes/Cargo.toml"),
            "[dependencies]\nrender.workspace = true\n",
        );
        let violations = check_product_manifests(&root).unwrap_err();
        assert!(violations.iter().any(|value| {
            value.code == "RPE-PURITY-0002" && value.manifest == root.join("Cargo.toml")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    fn write_manifest(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdf-rs-purity-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }
}
