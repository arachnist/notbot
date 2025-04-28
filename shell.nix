{
  mkShell,
  callPackage,
  rust-analyzer,
  rustfmt,
  clippy,
  cargo-mommy,
  cargo-udeps,
  tokio-console,
  cargo-depgraph,
  cargo-unused-features,
  treefmt,
  nixfmt-rfc-style,
  graphviz,
  ...
}:
mkShell {
  inputsFrom = [ (callPackage ./default.nix { }) ];
  buildInputs = [
    rust-analyzer
    rustfmt
    clippy
    cargo-mommy
    cargo-udeps
    cargo-depgraph
    graphviz
    tokio-console
    treefmt
    cargo-unused-features
    nixfmt-rfc-style
  ];
}
