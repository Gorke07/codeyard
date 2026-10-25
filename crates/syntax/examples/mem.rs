//! Memory measurement: how many MB does each step of syntax hold?
//!     cargo run --release -p syntax --example mem -- FILE

#[cfg(target_os = "macos")]
fn info() -> libc::rusage_info_v4 {
    // SAFETY: rusage_info_v4 is plain integers, so all-zero is valid.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live rusage_info_v4, which is what the RUSAGE_INFO_V4 flavor writes; the pid is our own.
    unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t) };
    info
}

#[cfg(target_os = "macos")]
fn footprint_mb() -> f64 {
    info().ri_phys_footprint as f64 / 1_048_576.0
}

#[cfg(target_os = "macos")]
fn peak_mb() -> f64 {
    info().ri_lifetime_max_phys_footprint as f64 / 1_048_576.0
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This memory profiling example requires macOS.");
}

#[cfg(target_os = "macos")]
fn main() {
    if std::env::var_os("MIMALLOC").is_some() {
        // SAFETY: nothing has parsed yet at this point.
        unsafe { syntax::use_mimalloc() };
    }
    let path = std::env::args().nth(1).expect("file");
    let base = footprint_mb();
    let mut last = base;
    let mut step = |name: &str| {
        let now = footprint_mb();
        println!("{name:<28} +{:>6.2} MB  (total +{:.2}, peak +{:.2})", now - last, now - base, peak_mb() - base);
        last = now;
    };
    let text = std::fs::read_to_string(&path).unwrap();
    let rope = ropey::Rope::from_str(&text);
    step(&format!("file + rope ({} KB)", text.len() / 1024));
    let lang = syntax::language_for_path(&path).expect("language");
    let s = syntax::Syntax::new(lang, &rope).unwrap();
    step("parse tree");
    let first = s.highlights(&rope, 0..rope.line_to_byte(40.min(rope.len_lines())));
    step("query compile + 40 lines");
    let mid = rope.len_lines() / 2;
    let spans = s.highlights(&rope, rope.line_to_byte(mid)..rope.line_to_byte((mid + 40).min(rope.len_lines())));
    step("second 40 lines");
    println!("({} + {} spans)", first.len(), spans.len());
}
