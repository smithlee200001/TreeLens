//! Dependency-direction guardrails and source compatibility for the refactor.
use std::{fs, path::Path};

fn visit_rust(path: &Path, check: &impl Fn(&Path, &str)) {
    for entry in fs::read_dir(path).expect("source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            visit_rust(&path, check);
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path.file_name().is_none_or(|name| name != "tests.rs")
        {
            let source = fs::read_to_string(&path).expect("Rust source");
            // Inline unit fixtures may legitimately exercise native APIs.
            let production = source.split("#[cfg(test)]").next().unwrap();
            check(&path, production);
        }
    }
}

#[test]
fn feature_and_ui_code_keep_native_apis_at_the_platform_boundary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for directory in ["features", "ui"] {
        visit_rust(&root.join(directory), &|path, source| {
            for line in source.lines() {
                let native = line.contains("std::os::")
                    || line.contains("windows::")
                    || line.contains("objc2_")
                    || line.contains("std::process::Command")
                    || (line.contains("cfg")
                        && !line.contains("test")
                        && ["windows", "unix", "target_os", "target_family"]
                            .iter()
                            .any(|target| line.contains(target)));
                assert!(
                    !native,
                    "native API or selection in {}: {line}",
                    path.display()
                );
            }
        });
    }
}

#[test]
fn platform_adapters_do_not_depend_on_features_or_views() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform");
    visit_rust(&root, &|path, source| {
        for forbidden in [
            "crate::features",
            "crate::ui",
            "crate::scanner",
            "crate::deletion",
            "crate::session",
            "crate::tree",
            "crate::footprint",
        ] {
            assert!(
                !source.contains(forbidden),
                "reverse dependency {forbidden} in {}",
                path.display()
            );
        }
    });
}

#[test]
fn legacy_public_paths_remain_the_same_types() {
    let session: tree_lens::session::Session =
        tree_lens::features::explorer::session::Session::new();
    assert!(!session.busy());
    let kind: tree_lens::scanner::NodeKind = tree_lens::features::scan::NodeKind::File;
    assert_eq!(kind, tree_lens::scanner::NodeKind::File);
    let mode: tree_lens::deletion::DeletionMode = tree_lens::features::delete::DeletionMode::Trash;
    assert_eq!(mode, tree_lens::deletion::DeletionMode::Trash);
    let sort: tree_lens::tree::SortBy = tree_lens::features::explorer::tree::SortBy::Name;
    assert_eq!(sort, tree_lens::tree::SortBy::Name);
    let tiles: Vec<tree_lens::footprint::Tile> =
        Vec::<tree_lens::features::explorer::footprint::Tile>::new();
    assert!(tiles.is_empty());
    assert!(!tree_lens::platform::TRASH_NAME.is_empty());
}
