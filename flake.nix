{
  outputs = {
    nixpkgs,
    self,
  }: let
    eachSys = f: builtins.mapAttrs (_system: pkgs: f pkgs) nixpkgs.legacyPackages;
  in {
    packages = eachSys (pkgs: {
      default = pkgs.callPackage ./. {};
      cross-arm = pkgs.pkgsCross.aarch64-multiplatform.callPackage ./. {};
      blinky = pkgs.callPackage ./blinky {};
      blinky-cross-arm = pkgs.pkgsCross.aarch64-multiplatform.callPackage ./blinky {};
    });
    devShells = eachSys (pkgs: {
      default = pkgs.mkShell {
        inputsFrom = [(self.packages.${pkgs.system}.default.overrideAttrs {auditable = false;})];
        packages = with pkgs; [
          rust-analyzer
          rustfmt
          cargo-watch
        ];
      };
    });
  };
}
