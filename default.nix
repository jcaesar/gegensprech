let
  name = "gegensprech";
  author = "jcaesar";
in
  {
    rustPlatform,
    fetchFromGitHub,
    lib,
    cmake,
    libopus,
    libpulseaudio,
    pkg-config,
    stdenv,
  }:
    rustPlatform.buildRustPackage rec {
      pname = name;
      version = "0.1.0";

      src = ./.;
      cargoLock = {
        lockFile = "${src}/Cargo.lock";
        outputHashes = {
          "ogg-opus-0.1.2" = "sha256-bHzM0xC8RiWD1wMFqJHpoaU43p3qR5lEwEsiYrzVY/A=";
        };
      };

      nativeBuildInputs = [cmake pkg-config];
      buildInputs = [libopus libpulseaudio];
      buildFeatures = ["audio-as-lib"];
      # libpulse-simple-sys links by name if pkgconfig fails,
      # and that results in a a binary that can't be run
      # postFixup = "${lib.getExe libtree} $out/bin/gegensprech";
      postFixup = lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
        $out/bin/gegensprech help
      '';

      meta = {
        description = "Matrix push-to-talk";
        license = lib.licenses.mit;
        platforms = lib.platforms.linux;
        maintainers = [lib.maintainers.${author}];
        homepage = "https://github.com/${author}/${name}";
        mainProgram = "${name}";
      };
    }
