{
  description = "cargo-athena — a Rust library + binary scaffolded with Nix";

  # fenix publishes prebuilt Rust toolchains to nix-community.cachix.org,
  # so `nix develop` *substitutes* the toolchain (a fast binary download)
  # instead of refetching the Rust dist + realizing wrappers — locally
  # AND in CI. Trusted Nix users pick this up automatically; others run
  # with `--accept-flake-config` (or add themselves to `trusted-users`).
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

        # Toolchain is defined once in ./rust-toolchain.toml so that
        # `cargo`/`rustup` and Nix agree on the exact same Rust. fenix's
        # `fromToolchainFile` wants a `sha256` over the resolved toolchain;
        # re-pin it (run `nix develop`, paste the hash Nix prints) whenever
        # rust-toolchain.toml or the fenix input changes.
        rustToolchain = pkgs.fenix.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-mvUGEOHYJpn3ikC5hckneuGixaC+yGrkMM/liDIDgoU=";
        };

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Single source of truth for the version (workspace Cargo.toml).
        version =
          (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        # One source of truth for the dev tooling, composed so each CI
        # job materializes only what it uses — the rest of the fat shell
        # is never substituted on that job's fresh runners.
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
        # Documentation site (docs/, published to GitHub Pages). No rust
        # — keep it out of the docs closure.
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
        # Lets rust-analyzer find the standard library sources.
        rustSrcEnv = "${rustToolchain}/lib/rustlib/src/rust/library";
      in
      {
        # `nix build` / `nix profile install github:mostlymaxi/cargo-athena`
        # / `nix run github:mostlymaxi/cargo-athena -- athena …`
        packages.default = rustPlatform.buildRustPackage {
          pname = "cargo-athena";
          inherit version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Just the CLI binary — the workspace also has the library
          # crates + examples we don't want in the install closure.
          cargoBuildFlags = [
            "--package"
            "cargo-athena"
            "--bin"
            "cargo-athena"
          ];
          # The workspace test suite needs docker/kind/trybuild and is
          # not a packaging concern; CI covers it.
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
        # `nix develop .#<x>` on a fresh runner skips the rest. One
        # source of truth — every one is a subset of the fat default.
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
