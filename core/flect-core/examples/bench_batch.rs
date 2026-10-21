//! Serial vs parallel timing for a batch of synthetic images.
//! `cargo run --release --example bench_batch -p flect-core -- [files=200] [WxH=1600x1200]`
//! Not part of the test run. Uses the same pool the engine uses.
use std::fs;
use std::path::Path;
use std::time::Instant;

use flect_core::folder::NullProgress;
use flect_core::pool;
use flect_core::settings::Settings;
use flect_core::tasks::{self, Ctx};
use image::{Rgba, RgbaImage};

fn photo(w: u32, h: u32, seed: u32) -> RgbaImage {
    let mut s = seed.wrapping_mul(2654435761).wrapping_add(1);
    RgbaImage::from_fn(w, h, |x, y| {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        Rgba([
            ((x * 255 / w) as u8).wrapping_add((s >> 29) as u8),
            ((y * 255 / h) as u8).wrapping_add((s >> 30) as u8),
            ((x ^ y) as u8).wrapping_add((s >> 28) as u8),
            255,
        ])
    })
}

fn run(task: &str, files: &[String], workers: usize, settings: &Settings) -> f64 {
    let t = Instant::now();
    let never = || false;
    let work = |i: usize| {
        let ctx = Ctx {
            settings,
            seven_zip: None,
            progress: &NullProgress,
            media: None,
            java: None,
            cfr: None,
        };
        tasks::run(task, Path::new(&files[i]), &ctx).map(|d| d.output)
    };
    let mut made = Vec::new();
    let n = pool::run_ordered(files.len(), workers, &never, &work, |u| {
        if let pool::Update::Finished(_, r) = u {
            made.push(r.expect("task ran"));
        }
    });
    assert_eq!(n, files.len());
    let secs = t.elapsed().as_secs_f64();
    for m in made {
        let _ = fs::remove_file(m);
    }
    secs
}

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(200);
    let (w, h) = args
        .next()
        .and_then(|s| {
            let (a, b) = s.split_once('x')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((1600u32, 1200u32));
    let dir = std::env::temp_dir().join(format!("flect-bench-batch-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let files: Vec<String> = (0..n)
        .map(|i| {
            let p = dir.join(format!("img{i:04}.png"));
            let img = photo(w, h, i as u32);
            img.save(&p).unwrap();
            p.to_string_lossy().into_owned()
        })
        .collect();
    let settings = Settings::default();
    println!(
        "{n} synthetic {w}x{h} PNGs, {} cores",
        std::thread::available_parallelism().map_or(1, usize::from)
    );
    for task in ["image.resize?scale=50", "image.convert?to=JPEG"] {
        let base = run(task, &files, 1, &settings);
        println!("{task}: serial {base:.2}s");
        for workers in [2, 4, 8] {
            let s = run(task, &files, workers, &settings);
            println!("  {workers} workers {s:.2}s  speedup x{:.2}", base / s);
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}
