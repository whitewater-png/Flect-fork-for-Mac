//! Rough timing for scan + duplicate detection. `cargo run --release --example bench_dup [files]`
use std::{fs, path::PathBuf, time::Instant};

use flect_core::folder::{find_duplicates, scan, ExclusionFilter, NullProgress};
use flect_core::settings::{DuplicateSettings, EmptyFolderSettings};

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let root = std::env::temp_dir().join(format!("flect-bench-{}", std::process::id()));
    for i in 0..n {
        let dir = root.join(format!("d{}", i % 100));
        fs::create_dir_all(&dir).unwrap();
        // every 5th file is a copy of another; sizes 1-300 KiB
        let seed = if i % 5 == 0 { i / 5 * 5 + 1 } else { i };
        let len = 1024 + (seed * 7919) % (300 * 1024);
        let data: Vec<u8> = (0..len).map(|j| ((j + seed) % 251) as u8).collect();
        fs::write(dir.join(format!("f{i}.bin")), data).unwrap();
    }
    let t = Instant::now();
    let s = scan(
        &[PathBuf::from(&root)],
        &ExclusionFilter::default(),
        &EmptyFolderSettings::default(),
        &NullProgress,
    )
    .unwrap();
    let scan_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let groups = find_duplicates(&s.files, &DuplicateSettings::default(), &NullProgress).unwrap();
    println!(
        "files={} bytes={}MiB scan={scan_ms:.0}ms dup={:.0}ms groups={}",
        s.files.len(),
        s.total_bytes() >> 20,
        t.elapsed().as_secs_f64() * 1e3,
        groups.len()
    );
    fs::remove_dir_all(&root).unwrap();
}
