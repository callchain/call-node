//! Extract Poseidon constants from poseidon-ark-no-std to Lean 4 definitions.
//!
//! This script reads the MDS matrices and round constants from the
//! poseidon-ark-no-std crate and emits Lean 4 `def` statements that
//! can be included in `formal_verification/lean/CallchainShielded/Poseidon.lean`.
//!
//! Usage: `cargo run --example extract_poseidon_constants 2>/dev/null > ../formal_verification/lean/CallchainShielded/PoseidonConstants.lean`
//!
//! Note: This is a standalone script. It depends on poseidon-ark-no-std being
//! available in the dependency tree.

use std::fmt::Write;

fn main() {
    let mut output = String::new();

    writeln!(&mut output, "import CallchainShielded.Fr").unwrap();
    writeln!(&mut output, "").unwrap();
    writeln!(&mut output, "namespace CallchainShielded").unwrap();
    writeln!(&mut output, "").unwrap();

    // TODO: Access poseidon-ark-no-std constants directly
    // The crate exposes `load_constants()` which returns:
    // - c: round constants (Vec<Vec<Fr>>)
    // - m: MDS matrices (Vec<Vec<Vec<Fr>>>)
    // - n_rounds_f: full rounds count
    // - n_rounds_p: partial rounds counts per width
    //
    // Since we cannot directly import poseidon-ark-no-std in a standalone script,
    // the constants need to be extracted at runtime or hardcoded.
    //
    // The constants are generated from the Poseidon reference implementation
    // and are deterministic for a given set of parameters (BN254, rate=8, R_F=8).

    writeln!(&mut output, "-- TODO: Extract actual constants from poseidon-ark-no-std").unwrap();
    writeln!(&mut output, "-- The constants below are placeholders.").unwrap();
    writeln!(&mut output, "").unwrap();
    writeln!(&mut output, "/-- Round constants for Poseidon(t=2) -/").unwrap();
    writeln!(&mut output, "def POSEIDON_RC_2 : List (List Fr) := []").unwrap();
    writeln!(&mut output, "").unwrap();
    writeln!(&mut output, "/-- MDS matrix for Poseidon(t=2) -/").unwrap();
    writeln!(&mut output, "def POSEIDON_MDS_2 : List (List Fr) := []").unwrap();
    writeln!(&mut output, "").unwrap();

    writeln!(&mut output, "end CallchainShielded").unwrap();

    println!("{}", output);
}
