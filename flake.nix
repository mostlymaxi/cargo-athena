{
  description = "cargo-athena: compile regular Rust into Argo Workflow YAML";

  nixConfig = {
    extra-substituters = [ "https://nix-community.cachix.org" ];
    extra-trusted-public-keys = [
      "nix-community.cachix.org-1:mB9FSh9qf2dCimDSUo8Zy7bkq5CX+/rkCWyvRCYg3Fs="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      fenix,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ fenix.overlays.default ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.fenix.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-A1abGIbOtcBSdrUMhDGrER3pRM1hQP4fp9gh3Y4PKc8=";
        };

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        version =
          (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        ciPackages = [ rustToolchain ]; # test / clippy / publish
        crossTools = [
          # static-musl cross-compilation for `cargo athena build`.
          pkgs.cargo-zigbuild
          pkgs.zig
        ];
        clusterTools = [
          # kind e2e: cluster + Argo + MinIO. Needs a host Docker or
          # Podman daemon (not a nix package — provided by the host).
          pkgs.kind
          pkgs.kubectl
          pkgs.argo-workflows
          pkgs.minio-client
          pkgs.jq
        ];

        docsPackages = [ pkgs.mdbook ];
        crossPackages = ciPackages ++ crossTools; # .#build (e2e build job)
        e2ePackages = ciPackages ++ clusterTools; # .#e2e  (e2e job)

        devPackages = # fat local default = union of all
          ciPackages
          ++ crossTools
          ++ clusterTools
          ++ docsPackages
          ++ [
            pkgs.cargo-watch
            pkgs.cargo-edit
            pkgs.cargo-nextest
          ];

        rustSrcEnv = "${rustToolchain}/lib/rustlib/src/rust/library";
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = "cargo-athena";
          inherit version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [
            "--package"
            "cargo-athena"
            "--bin"
            "cargo-athena"
          ];

          doCheck = false;

          meta = {
            description = "Compile regular Rust into Argo Workflow YAML";
            homepage = "https://github.com/mostlymaxi/cargo-athena";
            license = with pkgs.lib.licenses; [
              mit
              asl20
            ];
            mainProgram = "cargo-athena";
          };
        };

        apps.default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/cargo-athena";
        };

        devShells.default = pkgs.mkShell {
          packages = devPackages;
          env.RUST_SRC_PATH = rustSrcEnv;
          shellHook = ''
            echo "cargo-athena dev shell — $(rustc --version)"
          '';
        };

        # Lean per-job CI shells: each substitutes only its own tools, so
        # `nix develop .#<x>` on a fresh runner skips the rest.
        devShells.ci = pkgs.mkShell {
          # test / clippy / publish (compile only)
          packages = ciPackages;
          env.RUST_SRC_PATH = rustSrcEnv;
        };
        devShells.build = pkgs.mkShell {
          # e2e build job: `cargo athena build` (static-musl cross)
          packages = crossPackages;
          env.RUST_SRC_PATH = rustSrcEnv;
        };
        devShells.e2e = pkgs.mkShell {
          # e2e job: deploy.sh + e2e-test.sh (cargo + cluster tools)
          packages = e2ePackages;
          env.RUST_SRC_PATH = rustSrcEnv;
        };
        devShells.docs = pkgs.mkShell {
          # pages: mdbook only, no rust toolchain in the closure
          packages = docsPackages;
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
