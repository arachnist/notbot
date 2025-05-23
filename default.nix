{ lib, rustPlatform, pkg-config, openssl, sqlite, fontconfig, freetype, ... }:
let cargoToml = fromTOML (builtins.readFile (./. + "/Cargo.toml"));
in rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  version = cargoToml.package.version;
  cargoLock = { lockFile = ./Cargo.lock; };

  src = lib.cleanSource ./.;
  nativeBuildInputs = [ pkg-config ];

  RUSTFLAGS = "--cfg tokio_unstable";

  buildInputs = [ openssl sqlite fontconfig freetype ];

  postInstall = ''
    cp -r $src/webui $out/webui
  '';
}
