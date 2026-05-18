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
          sha256 = "sha256-gh/xTkxKHL4eiRXzWv8KP7vfjSk61Iq48x47BEDFgfk=";
        };

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Single source of truth for the version (workspace Cargo.toml).
        version =
          (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

        # One source of truth for the dev tooling. The full shell is the
        # lean CI shell PLUS the e2e/docs tooling. CI's test/clippy only
        # compile, so they use `.#ci` and skip substituting the ~10 heavy
        # tools (kind/kubectl/argo/zig/mdbook/…) on every fresh runner.
        ciPackages = [ rustToolchain ];
        devPackages = ciPackages ++ [
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

        # Lean shell for compile-only CI (test/clippy): toolchain only,
        # so `nix develop .#ci` doesn't substitute the e2e/docs closure.
        devShells.ci = pkgs.mkShell {
          packages = ciPackages;
          env.RUST_SRC_PATH = rustSrcEnv;
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
