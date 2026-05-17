{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

      in
      {
        packages = {
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
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer

            turso
            pnpm
          ];
        };
      }
    );
}
