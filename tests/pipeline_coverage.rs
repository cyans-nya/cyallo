//! How much real code the HIR/MIR/AIR pipeline accepts, and the reasons for the rest.
//!
//! Run `cargo test --test pipeline_coverage -- --nocapture` for the report. The test only
//! asserts that coverage does not fall below the recorded floor, so a change that makes
//! the pipeline decline more code is noticed.
use cyallo_ir::analysis::IndexedRoutineBody;
use std::{collections::BTreeMap, path::Path};

fn sources(directory: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // The startup workloads are generated, thousands of near-identical routines.
            if path.ends_with("startup") {
                continue;
            }
            sources(&path, out);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "ya" || extension == "yas")
        {
            out.push(path);
        }
    }
}

#[test]
fn report_what_the_pipeline_accepts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for directory in ["examples", "std"] {
        sources(&root.join(directory), &mut files);
    }
    files.sort();
    let (mut accepted, mut total) = (0usize, 0usize);
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(file) = cyallo_parser::parse_modern_snapshot(text.into(), false, cyallo::Jobs(1))
        else {
            continue;
        };
        for handle in file.units().iter().flat_map(|unit| unit.bodies()) {
            total += 1;
            match IndexedRoutineBody::decline_reason(&handle) {
                None => accepted += 1,
                Some(reason) => {
                    if std::env::var_os("COVERAGE_VERBOSE").is_some() {
                        let span = handle.function().span;
                        let text = &file.source().text()[span.start as usize..span.end as usize];
                        println!(
                            "DECLINED {} [{reason}] {}",
                            path.strip_prefix(root).unwrap_or(path).display(),
                            text.lines().next().unwrap_or_default()
                        );
                    }
                    *reasons.entry(reason).or_default() += 1;
                }
            }
        }
    }
    let mut ranked: Vec<_> = reasons.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!(
        "routines accepted: {accepted} of {total} in {} files",
        files.len()
    );
    for (reason, count) in ranked.iter().take(40) {
        println!("{count:6}  {reason}");
    }
    assert!(total >= 50, "the corpus shrank: {total} routines");
    // The recorded floor: raise it when coverage grows, and look into any drop.
    assert!(
        accepted >= 97,
        "coverage fell to {accepted} of {total} routines"
    );
}
