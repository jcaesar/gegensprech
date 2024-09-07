{
  outputs =
    {
      nixpkgs,
      self,
    }:
    let
      eachSys =
        f:
        nixpkgs.lib.genAttrs
          [
            "x86_64-linux"
            "aarch64-linux"
          ]
          (
            system:
            f (
              import nixpkgs {
                inherit system;
              }
            )
          );
    in
    {
      packages = eachSys (pkgs: {
        default = pkgs.callPackage ./default.nix {};
        cross-arm = pkgs.pkgsCross.aarch64-multiplatform.callPackage ./default.nix {};
      });
      devShells = eachSys (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ (self.packages.${pkgs.system}.default.overrideAttrs { auditable = false; }) ];
          packages = with pkgs; [
            rust-analyzer
            rustfmt
            cargo-watch
            cargo-watch
          ];
        };
      });
    };
}
