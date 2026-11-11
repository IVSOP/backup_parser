{
  lib,
  rustPlatform,
  makeBinaryWrapper,
  restic,
  gdu,
}:
rustPlatform.buildRustPackage {
  pname = "backup_parser";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../tests
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ makeBinaryWrapper ];
  nativeCheckInputs = [ restic ];
  doCheck = true;
  postInstall = ''
    wrapProgram "$out/bin/backup_parser" \
      --prefix PATH : ${
        lib.makeBinPath [
          restic
          gdu
        ]
      }
  '';

  meta = {
    description = "Back up typed JSONC path manifests with restic";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "backup_parser";
  };
}
