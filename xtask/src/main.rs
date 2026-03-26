use anyhow::{bail, Context, Result};
use clap::Parser;
use std::process::Command;

#[derive(Parser)]
enum Cli {
    /// Build the eBPF kernel-side program with nightly + bpf-linker.
    BuildEbpf {
        /// Build profile: "dev" or "release".
        #[clap(long, default_value = "release")]
        profile: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli {
        Cli::BuildEbpf { profile } => build_ebpf(&profile),
    }
}

fn build_ebpf(profile: &str) -> Result<()> {
    let mut args = vec![
        "+nightly",
        "build",
        "--manifest-path",
        "ci-tracer-ebpf/Cargo.toml",
        "--target",
        "bpfel-unknown-none",
        "-Z",
        "build-std=core",
        "--target-dir",
        "target",
    ];

    if profile == "release" {
        args.push("--release");
    }

    let status = Command::new("cargo")
        .args(&args)
        .status()
        .context("failed to run cargo — is the nightly toolchain installed?")?;

    if !status.success() {
        bail!("eBPF build failed");
    }

    eprintln!("eBPF program built → target/bpfel-unknown-none/{profile}/ci-tracer-ebpf");
    Ok(())
}
