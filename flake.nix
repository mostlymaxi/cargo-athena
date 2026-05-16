{
  description = "cargo-athena — a Rust library + binary scaffolded with Nix";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Toolchain is defined once in ./rust-toolchain.toml so that
        # `cargo`/`rustup` and Nix agree on the exact same Rust.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = "cargo-athena";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # protoc is needed by cargo-athena-api's prost build script.
          nativeBuildInputs = [ pkgs.protobuf ];
          PROTOC = "${pkgs.protobuf}/bin/protoc";
          meta = {
            description = "Compile regular Rust into Argo Workflow YAML";
            mainProgram = "cargo-athena";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.cargo-watch
            pkgs.cargo-edit
            pkgs.cargo-nextest
            # protobuf toolchain for cargo-athena-api codegen.
            pkgs.protobuf
            pkgs.buf
            # static-musl cross-compilation for `cargo athena build`.
            pkgs.cargo-zigbuild
            pkgs.zig
            # kind e2e: cluster + Argo + MinIO. Needs a host Docker or
            # Podman daemon (not a nix package — provided by the host).
            pkgs.kind
            pkgs.kubectl
            pkgs.argo-workflows
            pkgs.minio-client
            pkgs.jq
          ];

          # Lets rust-analyzer find the standard library sources.
          env.RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          # prost-build (cargo-athena-api) shells out to protoc.
          env.PROTOC = "${pkgs.protobuf}/bin/protoc";

          shellHook = ''
            echo "cargo-athena dev shell — $(rustc --version)"
          '';
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
