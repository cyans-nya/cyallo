//! Absolute lex / index / parse cost of the standard library sources, the upper
//! bound of what a fresh process spends in the parser front end.
//! `cargo +nightly bench -p cyallo-parser --bench startup_share`
use cyallo_parser::{index_modern, lex_modern, parse};
use std::hint::black_box;
use std::time::Instant;

fn median_us(mut f: impl FnMut(), iters: usize) -> f64 {
    let mut s: Vec<f64> = (0..15)
        .map(|_| {
            let t = Instant::now();
            for _ in 0..iters {
                f();
            }
            t.elapsed().as_secs_f64() * 1e6 / iters as f64
        })
        .collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut sources = vec![std::fs::read_to_string(root.join("std.ya")).unwrap()];
    let mut names: Vec<_> = std::fs::read_dir(root.join("std"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    names.sort();
    for p in names
        .iter()
        .filter(|p| p.extension().is_some_and(|x| x == "ya"))
    {
        sources.push(std::fs::read_to_string(p).unwrap());
    }
    let bytes: usize = sources.iter().map(String::len).sum();
    let fib = std::fs::read_to_string(root.join("examples/fib.yas")).unwrap();
    println!("std sources: {} files, {} bytes", sources.len(), bytes);
    let lex = median_us(
        || {
            for s in &sources {
                black_box(lex_modern(black_box(s)).ok());
            }
        },
        200,
    );
    let index = median_us(
        || {
            for s in &sources {
                black_box(index_modern(black_box(s)).ok());
            }
        },
        200,
    );
    let full = median_us(
        || {
            for s in &sources {
                black_box(parse(black_box(s)).ok());
            }
        },
        100,
    );
    let fib_full = median_us(
        || {
            black_box(parse(black_box(&fib)).ok());
        },
        2000,
    );
    println!(
        "lex           {lex:8.1} us  ({:.2} GB/s)",
        bytes as f64 / lex / 1e3
    );
    println!("lex + index   {index:8.1} us");
    println!("full parse    {full:8.1} us");
    println!("fib.ya parse  {fib_full:8.1} us");
}
