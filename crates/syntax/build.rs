fn main() {
    let dir = std::path::Path::new("grammars/dockerfile");
    cc::Build::new()
        .include(dir)
        .file(dir.join("parser.c"))
        .file(dir.join("scanner.c"))
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-unused-but-set-variable")
        .flag_if_supported("-Wno-trigraphs")
        .compile("tree_sitter_dockerfile");
    println!("cargo:rerun-if-changed=grammars/dockerfile");
}
