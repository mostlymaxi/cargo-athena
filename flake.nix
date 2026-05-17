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

        # Single source of truth for the version (workspace Cargo.toml).
        version =
          (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
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
          packages = [
            rustToolchain
            pkgs.cargo-watch
            pkgs.cargo-edit
            pkgs.cargo-nextest
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
            # Documentation site (docs/, published to GitHub Pages).
            pkgs.mdbook
          ];

          # Lets rust-analyzer find the standard library sources.
          env.RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            echo "cargo-athena dev shell — $(rustc --version)"
          '';
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
