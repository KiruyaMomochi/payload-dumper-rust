{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils, naersk }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };
      in
      {
        defaultPackage = naersk-lib.buildPackage {
          src = ./.;
          buildInputs = [ pkgs.protobuf ];
        };
        devShell = with pkgs; mkShell {
          buildInputs = [
            cargo
            cargo-outdated
            rustc
            rustfmt
            pre-commit
            rustPackages.clippy
            rust-analyzer
            protobuf
          ];
          RUST_SRC_PATH = rustPlatform.rustLibSrc;
        };
      });
}
