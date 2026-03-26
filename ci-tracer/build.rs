use std::path::PathBuf;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let bpf_obj = PathBuf::from(&manifest_dir)
        .join("../target/bpfel-unknown-none/release/ci-tracer-ebpf");

    println!("cargo:rerun-if-changed={}", bpf_obj.display());

    if !bpf_obj.exists() {
        panic!(
            "BPF object not found at {path}. Run `cargo xtask build-ebpf` first.",
            path = bpf_obj.display()
        );
    }
}
