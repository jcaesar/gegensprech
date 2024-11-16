let
  name = "twomic-led-blinky";
  author = "jcaesar";
in
  {
    rustPlatform,
    lib,
  }:
    rustPlatform.buildRustPackage rec {
      pname = name;
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

      src = ./.;
      cargoLock.lockFile = "${src}/Cargo.lock";

      meta = {
        description = "Blink Seeed 2mic leds, possibly in initrd";
        license = lib.licenses.mit;
        platforms = lib.platforms.linux;
        maintainers = [lib.maintainers.${author}];
        homepage = "https://github.com/${author}/${name}";
        mainProgram = "${name}";
      };
    }
