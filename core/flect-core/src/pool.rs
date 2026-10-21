//! Bounded parallelism for batches of independent, CPU-bound, per-file tasks.
//!
//! Only image and text tasks qualify (see [`is_parallel_task`]): ffmpeg already uses every
//! core, folder-wide tasks look at the whole selection at once, and archives run external
//! tools. Workers claim input indices in order, so files start in input order; results are
//! handed back to the caller's thread, which delivers them in input order. Everything the
//! user sees (progress, outputs, errors) therefore comes from one place and stays
//! monotonic, exactly as in a serial run.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

/// Workers used when nothing is configured. Decoded images are large, so this stays modest.
pub const DEFAULT_MAX_WORKERS: usize = 4;
/// Upper bound for an explicit request (`FLECT_JOBS` or the `jobs` run option).
pub const HARD_MAX_WORKERS: usize = 16;

/// Image and text tasks write one exclusive-create file per input and share no state.
pub fn is_parallel_task(name: &str) -> bool {
    crate::tasks::is_text_task(name) || crate::tasks::is_image_task(name)
}

/// How many workers to use for `files` inputs. `requested` (run option) wins over the
/// `FLECT_JOBS` environment variable, which wins over `min(cores, 4)`. 0 or garbage means
/// "default"; 1 means serial. Never more than the file count or [`HARD_MAX_WORKERS`].
pub fn worker_count(files: usize, requested: Option<usize>) -> usize {
    let env = std::env::var("FLECT_JOBS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok());
    let wanted = requested
        .filter(|n| *n > 0)
        .or(env.filter(|n| *n > 0))
        .map(|n| n.min(HARD_MAX_WORKERS))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(DEFAULT_MAX_WORKERS)
        });
    wanted.min(files).max(1)
}

/// What the caller's thread sees while a batch runs.
pub enum Update<R> {
    /// File `i` is the next one whose result is awaited (emit "processing" here).
    Waiting(usize),
    /// File `i` finished. Called strictly in input order, once per finished file.
    Finished(usize, R),
}

/// Run `work(i)` for `i in 0..n` on `workers` threads. `cancelled` is polled before every
/// dispatch: once true nothing new starts, files already running finish, and their results
/// are still delivered (a gapless prefix of the input, in order) so the caller can report
/// what was created. `on_update` runs on the calling thread only. Returns how many files
/// were delivered.
pub fn run_ordered<R: Send>(
    n: usize,
    workers: usize,
    cancelled: &(dyn Fn() -> bool + Sync),
    work: &(dyn Fn(usize) -> R + Sync),
    mut on_update: impl FnMut(Update<R>),
) -> usize {
    let workers = workers.clamp(1, n.max(1));
    let next = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel::<(usize, R)>();
    let mut delivered = 0usize;
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let next = &next;
            scope.spawn(move || loop {
                if cancelled() {
                    break;
                }
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= n {
                    break;
                }
                if tx.send((i, work(i))).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut pending: BTreeMap<usize, R> = BTreeMap::new();
        while delivered < n {
            on_update(Update::Waiting(delivered));
            while !pending.contains_key(&delivered) {
                match rx.recv() {
                    Ok((i, r)) => {
                        pending.insert(i, r);
                    }
                    // Every worker stopped (cancelled or panicked): nothing more will come.
                    Err(_) => return,
                }
            }
            if let Some(r) = pending.remove(&delivered) {
                on_update(Update::Finished(delivered, r));
                delivered += 1;
            }
        }
    });
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn results_arrive_in_input_order_whatever_the_finish_order() {
        let never = || false;
        // Early files are slow, late ones fast: they finish out of order.
        let work = |i: usize| {
            std::thread::sleep(Duration::from_millis(((20 - i) * 2) as u64));
            i * 10
        };
        let mut seen = Vec::new();
        let n = run_ordered(20, 6, &never, &work, |u| {
            if let Update::Finished(i, r) = u {
                seen.push((i, r));
            }
        });
        assert_eq!(n, 20);
        assert_eq!(seen, (0..20).map(|i| (i, i * 10)).collect::<Vec<_>>());
    }

    #[test]
    fn waiting_is_monotonic_and_every_file_finishes_once() {
        let never = || false;
        let mut waits = Vec::new();
        let mut finished = Vec::new();
        run_ordered(8, 3, &never, &|i| i, |u| match u {
            Update::Waiting(i) => waits.push(i),
            Update::Finished(i, _) => finished.push(i),
        });
        assert!(waits.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(finished, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn never_more_than_the_worker_count_run_at_once() {
        let never = || false;
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        run_ordered(
            40,
            4,
            &never,
            &|_| {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(3));
                live.fetch_sub(1, Ordering::SeqCst);
            },
            |_| {},
        );
        let p = peak.load(Ordering::SeqCst);
        assert!((1..=4).contains(&p), "peak {p}");
    }

    #[test]
    fn cancel_stops_dispatching_and_delivers_a_prefix_of_finished_files() {
        let flag = AtomicBool::new(false);
        let cancelled = || flag.load(Ordering::SeqCst);
        let started = Mutex::new(Vec::new());
        let work = |i: usize| {
            started.lock().unwrap().push(i);
            if i == 5 {
                flag.store(true, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(10));
            i
        };
        let mut seen = Vec::new();
        let n = run_ordered(1000, 3, &cancelled, &work, |u| {
            if let Update::Finished(i, _) = u {
                seen.push(i);
            }
        });
        let started = started.into_inner().unwrap();
        assert!(started.len() < 20, "dispatching stopped: {}", started.len());
        assert_eq!(seen, (0..n).collect::<Vec<_>>(), "a gapless prefix");
        assert_eq!(n, started.len(), "every started file was delivered");
    }

    /// Many threads asking for the same destination name never collide: every call gets its
    /// own file, numbered ` (2)`, ` (3)`..., and nobody's bytes are overwritten.
    #[test]
    fn racing_writers_for_one_destination_get_distinct_exclusive_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        std::fs::write(&src, b"src").unwrap();
        let threads = 8usize;
        let each = 25usize;
        let barrier = std::sync::Barrier::new(threads);
        let results = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for t in 0..threads {
                let (barrier, results, src) = (&barrier, &results, &src);
                s.spawn(move || {
                    barrier.wait();
                    for k in 0..each {
                        let body = format!("writer {t} #{k}");
                        let out = crate::tasks::write_result(src, "", "_x", None, body.as_bytes())
                            .expect("a free name is always found");
                        results.lock().unwrap().push((out, body));
                    }
                });
            }
        });
        let results = results.into_inner().unwrap();
        assert_eq!(results.len(), threads * each);
        let names: std::collections::HashSet<_> = results.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(names.len(), results.len(), "no two writers got one name");
        for (p, body) in &results {
            assert_eq!(&std::fs::read_to_string(p).unwrap(), body, "{p:?} intact");
        }
        // Numbering is gapless: _x, _x (2) ... _x (N).
        for n in 1..=results.len() {
            let name = if n == 1 {
                "a_x.txt".to_owned()
            } else {
                format!("a_x ({n}).txt")
            };
            assert!(dir.path().join(&name).exists(), "{name} missing");
        }
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .count();
        assert_eq!(leftovers, 0, "no scratch files left behind");
    }

    #[test]
    fn worker_count_rules() {
        assert_eq!(worker_count(1, Some(8)), 1);
        assert_eq!(worker_count(0, None), 1);
        assert_eq!(worker_count(100, Some(3)), 3);
        assert_eq!(worker_count(100, Some(1000)), HARD_MAX_WORKERS);
        assert!(worker_count(100, None) >= 1);
    }

    #[test]
    fn only_image_and_text_tasks_qualify() {
        for yes in [
            "image.resize",
            "image.convert",
            "text.encode",
            "json.pretty",
        ] {
            assert!(is_parallel_task(yes), "{yes}");
        }
        for no in [
            "video.compress",
            "audio.extract",
            "archive.extract",
            "folder.compress",
            "jar.decompile",
            "folder.rename",
        ] {
            assert!(!is_parallel_task(no), "{no}");
        }
    }
}
