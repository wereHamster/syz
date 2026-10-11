{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    {
      self,
      nixpkgs,
      ...
    }:
    let
      forAllSystems =
        function: nixpkgs.lib.genAttrs nixpkgs.lib.systems.flakeExposed (system: function nixpkgs.legacyPackages.${system});

    in
    {
      nixosModules = rec {
        default = syzd;
        syzd = import ./nix/module.nix self;
      };

      packages = forAllSystems (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "syz";
          version = "0.1.0";

          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            perl
            gnumake
            makeWrapper
          ];

          doCheck = false;

          # The ecosystem scanners shell out to these tools when applying
          # dependency updates.
          postFixup = ''
            wrapProgram $out/bin/syzd \
              --prefix PATH : ${
                pkgs.lib.makeBinPath (
                  with pkgs;
                  [
                    nix
                    cargo
                    git
                    pnpm
                  ]
                )
              }
          '';

          meta = {
            mainProgram = "syzd";
          };
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
