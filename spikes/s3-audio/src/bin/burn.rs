//! CPU load generator for the S3 load tests: `burn <threads> <secs>` spins N normal-priority
//! threads (SCHED_OTHER) for the given time. (`stress` on this machine is Go's test-stress tool,
//! not the CPU stressor, so the spike carries its own.)
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(16);
    let secs: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let hs: Vec<_> = (0..n)
        .map(|i| {
            std::thread::spawn(move || {
                let mut x = i as u64 + 1;
                while std::time::Instant::now() < end {
                    for _ in 0..100_000 {
                        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    }
                    std::hint::black_box(x);
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
}
