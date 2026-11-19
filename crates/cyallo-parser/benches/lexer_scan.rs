//! Lexer scan-variant benchmark. Run:
//! `cargo +nightly bench -p cyallo-parser --features memchr-control --bench lexer_scan`
use cyallo_parser::{ScanVariant, lex_variant};
use std::hint::black_box;
use std::time::Instant;

const VARIANTS: [(&str, ScanVariant); 8] = [
    ("original", ScanVariant::Original),
    ("scalar-runs", ScanVariant::ScalarRuns),
    ("comment-simd", ScanVariant::CommentSimd),
    ("ws-simd", ScanVariant::WhitespaceSimd),
    ("both-simd", ScanVariant::BothSimd),
    ("comment+hash-kw", ScanVariant::CommentSimdHashed),
    ("both-hybrid", ScanVariant::BothHybrid),
    ("comment-memchr", ScanVariant::CommentMemchr),
];

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "ya") {
            out.push(p);
        }
    }
}

/// Median ns per full pass over `sources`, variants rotated across samples.
fn measure(sources: &[String], iters: usize, samples: usize) -> Vec<f64> {
    let mut times = vec![Vec::new(); VARIANTS.len()];
    for s in 0..samples {
        for k in 0..VARIANTS.len() {
            let i = (k + s) % VARIANTS.len();
            let v = VARIANTS[i].1;
            let t = Instant::now();
            for _ in 0..iters {
                for src in sources {
                    black_box(lex_variant(black_box(src), v).ok());
                }
            }
            times[i].push(t.elapsed().as_nanos() as f64 / iters as f64);
        }
    }
    times
        .into_iter()
        .map(|mut t| {
            t.sort_by(|a, b| a.partial_cmp(b).unwrap());
            t[t.len() / 2]
        })
        .collect()
}

fn report(name: &str, sources: &[String], iters: usize) {
    let bytes: usize = sources.iter().map(String::len).sum();
    let r = measure(sources, iters, 15);
    print!("{name:<34} {:>8} B {:>4} files |", bytes, sources.len());
    for ((label, _), ns) in VARIANTS.iter().zip(&r) {
        print!(" {label} {:.0}ns ({:.2} GB/s) |", ns / sources.len() as f64, bytes as f64 / ns);
    }
    println!();
    let base = r[0];
    print!("{:<34} speedup vs original:", "");
    for ((label, _), ns) in VARIANTS.iter().zip(&r) {
        print!(" {label} {:.2}x", base / ns);
    }
    println!();
}

fn main() {
    println!("host: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for d in ["examples", "std", "tests"] {
        walk(&root.join(d), &mut files);
    }
    files.push(root.join("std.ya"));
    files.sort();
    let real: Vec<String> = files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).collect();
    let mut by_size = real.clone();
    by_size.sort_by_key(String::len);
    report("real: all .ya", &real, 2000);
    report("real: smallest 10", &by_size[..10], 20000);
    report("real: largest 5", &by_size[by_size.len() - 5..], 4000);
    let big = real.join("\n");
    report("real: concatenated", std::slice::from_ref(&big), 2000);

    for n in [2usize, 8, 16, 32, 64, 256, 4096] {
        let c: String = (0..200).map(|_| format!("x // {}\n", "c".repeat(n))).collect();
        report(&format!("gen: 200 comments of {n} B"), &[c], 2000);
    }
    for n in [1usize, 2, 4, 8, 16, 32, 64, 256, 4096] {
        let w: String = (0..200).map(|_| format!("x{}y\n", " ".repeat(n))).collect();
        report(&format!("gen: 200 blank runs of {n} B"), &[w], 2000);
    }
    // Typical indentation: LF then 4/8/12 spaces then token.
    let ind: String = (0..300).map(|i| format!("\n{}x", " ".repeat(4 * (1 + i % 3)))).collect();
    report("gen: indented lines (4/8/12)", &[ind], 5000);
}
