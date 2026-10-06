{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    {
      nixpkgs,
      ...
    }:
    let
      forAllSystems =
        function: nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed (system: function nixpkgs.legacyPackages.${system});

    in
    {
      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "syz";
          version = "0.1.0";

          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            perl
            gnumake
          ];

          doCheck = false;
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer

            turso
            nodejs
            pnpm
          ];
        };

        agent = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer

            turso
            nodejs
            pnpm
            biome
          ];
        };

        workflow = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
          ];
        };
      });
    };
}
