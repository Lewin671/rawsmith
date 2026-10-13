//! Opt-in process allocation accounting and stage diagnostics on stderr.
//! Build with `--features render-profile`, then set RAWSMITH_PROFILE=1.
use std::time::Instant;

#[cfg(feature = "render-profile")]
mod allocations {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    pub static LIVE: AtomicUsize = AtomicUsize::new(0);
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);
    pub static TOTAL: AtomicUsize = AtomicUsize::new(0);
    struct Tracked;
    #[global_allocator]
    static ALLOCATOR: Tracked = Tracked;
    fn add(n: usize) {
        TOTAL.fetch_add(n, Relaxed);
        let live = LIVE.fetch_add(n, Relaxed) + n;
        PEAK.fetch_max(live, Relaxed);
    }
    unsafe impl GlobalAlloc for Tracked {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc(layout);
            if !p.is_null() {
                add(layout.size());
            }
            p
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let p = System.alloc_zeroed(layout);
            if !p.is_null() {
                add(layout.size());
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
            LIVE.fetch_sub(layout.size(), Relaxed);
            System.dealloc(p, layout);
        }
        unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let out = System.realloc(p, layout, size);
            if !out.is_null() {
                LIVE.fetch_sub(layout.size(), Relaxed);
                add(size);
            }
            out
        }
    }
}

pub struct Stage {
    name: &'static str,
    started: Option<Instant>,
    allocated: usize,
}
impl Stage {
    pub fn new(name: &'static str) -> Self {
        let enabled = std::sync::OnceLock::get_or_init(&ENABLED, || std::env::var_os("RAWSMITH_PROFILE").is_some());
        Self { name, started: enabled.then(Instant::now), allocated: totals().0 }
    }
}
static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
fn totals() -> (usize, usize, usize) {
    #[cfg(feature = "render-profile")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        (allocations::TOTAL.load(Relaxed), allocations::LIVE.load(Relaxed), allocations::PEAK.load(Relaxed))
    }
    #[cfg(not(feature = "render-profile"))]
    {
        (0, 0, 0)
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            let (allocated, live, peak) = totals();
            eprintln!(
                "profile\t{}\t{:.3}\t{}\t{}\t{}",
                self.name,
                started.elapsed().as_secs_f64() * 1000.0,
                allocated - self.allocated,
                live,
                peak
            );
        }
    }
}
