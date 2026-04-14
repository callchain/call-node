//! xtask: Build automation for call-node
//!
//! Usage:
//!   cargo x lint        — fmt + clippy + deny
//!   cargo x test        — workspace tests + doctests
//!   cargo x build       — release build
//!   cargo x bench       — benchmark suite

use xshell::{Shell, cmd};

fn main() -> anyhow::Result<()> {
    let sh = Shell::new()?;
    let Some(task) = std::env::args().nth(1) else {
        println!("Usage: cargo x <task>");
        println!("  lint   — fmt + clippy + deny");
        println!("  test   — workspace tests + doctests");
        println!("  build  — release build");
        println!("  bench  — benchmark suite");
        return Ok(());
    };

    match task.as_str() {
        "lint" => lint(&sh)?,
        "test" => test(&sh)?,
        "build" => build(&sh)?,
        "bench" => bench(&sh)?,
        other => {
            eprintln!("Unknown task: {other}");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn lint(sh: &Shell) -> anyhow::Result<()> {
    println!("=== cargo fmt ===");
    cmd!(sh, "cargo fmt --all").run()?;

    println!("\n=== cargo clippy ===");
    cmd!(sh, "cargo clippy --workspace --all-features -- -D warnings").run()?;

    println!("\n=== cargo deny ===");
    cmd!(sh, "cargo deny check").run()?;

    println!("\nAll lint checks passed.");
    Ok(())
}

fn test(sh: &Shell) -> anyhow::Result<()> {
    println!("=== cargo test ===");
    cmd!(sh, "cargo test --workspace --all-features").run()?;

    println!("\n=== cargo test --doc ===");
    cmd!(sh, "cargo test --workspace --doc").run()?;

    println!("\nAll tests passed.");
    Ok(())
}

fn build(sh: &Shell) -> anyhow::Result<()> {
    println!("=== cargo build --release ===");
    cmd!(sh, "cargo build --release").run()?;
    println!("\nBuild complete.");
    Ok(())
}

fn bench(sh: &Shell) -> anyhow::Result<()> {
    println!("=== cargo bench ===");
    cmd!(sh, "cargo bench --workspace").run()?;
    Ok(())
}
