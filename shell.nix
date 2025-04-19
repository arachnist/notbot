{ mkShell, callPackage, rust-analyzer, rustfmt, clippy, cargo-mommy, cargo-udeps
, ... }:
mkShell {
  # Get dependencies from the main package
  inputsFrom = [ (callPackage ./default.nix { }) ];
  # Additional tooling
  buildInputs = [ rust-analyzer rustfmt clippy cargo-mommy cargo-udeps ];
}
