{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
  sqlite,
  luajit_2_1,
  ...
}:
let
  cargoToml = fromTOML (builtins.readFile (./. + "/Cargo.toml"));
in
rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;
  cargoLock = {
    lockFile = ./Cargo.lock;
    outputHashes = {
      "openidconnect-3.5.0" = "sha256-bz90Kvericq7q8UX9JjoYELMntotAYQKTOl0FgCW6A0=";
    };
  };

  src = lib.cleanSource ./.;
  nativeBuildInputs = [ pkg-config ];

  buildInputs = [
    openssl
    sqlite
    luajit_2_1
  ];
}
