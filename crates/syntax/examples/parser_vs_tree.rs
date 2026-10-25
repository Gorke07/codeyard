//! Parse cost: the tree, or the parser's internal buffers?
fn footprint_mb() -> f64 {
    // SAFETY: rusage_info_v4 is plain integers, so all-zero is valid.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a live rusage_info_v4, which is what the RUSAGE_INFO_V4 flavor writes; the pid is our own.
    unsafe { libc::proc_pid_rusage(libc::getpid(), libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t) };
    info.ri_phys_footprint as f64 / 1_048_576.0
}

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let base = footprint_mb();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
    let tree = parser.parse(&text, None).unwrap();
    let after_parse = footprint_mb();
    drop(parser);
    let after_drop = footprint_mb();
    let mut count = 0usize;
    let mut cur = tree.walk();
    'outer: loop {
        count += 1;
        if cur.goto_first_child() {
            continue;
        }
        while !cur.goto_next_sibling() {
            if !cur.goto_parent() {
                break 'outer;
            }
        }
    }
    println!(
        "parse: +{:.2} MB, after the parser is dropped: +{:.2} MB (tree), {} nodes, {:.0} bytes/node",
        after_parse - base,
        after_drop - base,
        count,
        (after_drop - base) * 1_048_576.0 / count as f64
    );
    drop(cur);
    drop(tree);
    println!("after the tree is dropped too: +{:.2} MB", footprint_mb() - base);
}
