{
  description = "Tap | A hardware-accelerated GPU terminal emulator";

  # Binary cache populated by CI on every merge to main; nix offers to
  # enable it on first use so `nix run github:raphamorim/tap` becomes a
  # download instead of a build.
  nixConfig = {
    extra-substituters = ["https://tapterm.cachix.org"];
    extra-trusted-public-keys = ["tapterm.cachix.org-1:cs/H9Jf0ZpHyR4WgjoNJZVBpkVi69Y4JASUK5ReEQPE="];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
    systems.url = "github:nix-systems/default";
  };

  outputs = inputs @ {flake-parts, ...}:
    flake-parts.lib.mkFlake {inherit inputs;} {
      imports = [flake-parts.flakeModules.easyOverlay];

      systems = import inputs.systems;

      perSystem = {
        self',
        inputs',
        pkgs,
        system,
        lib,
        ...
      }: let
        # Defines a devshell using the `rust-toolchain`, allowing for
        # different versions of rust to be used.
        mkDevShell = rust-toolchain: let
          runtimeDeps = self'.packages.tap.runtimeDependencies;
          tools =
            self'.packages.tap.nativeBuildInputs ++ self'.packages.tap.buildInputs ++ [rust-toolchain];
        in
          pkgs.mkShell {
            packages = [self'.formatter] ++ tools;
            LD_LIBRARY_PATH = "${lib.makeLibraryPath runtimeDeps}";
          };
        toolchains = rec {
          msrv = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          stable = pkgs.rust-bin.stable.latest.minimal;
          nightly = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.minimal);
          tap = msrv;
          default = tap;
        };
      in {
        formatter = pkgs.alejandra;
        _module.args.pkgs = import inputs.nixpkgs {
          inherit system;
          overlays = [(import inputs.rust-overlay)];
        };

        # Create overlay to override `tap` with this flake's default
        overlayAttrs = {inherit (self'.packages) tap;};
        packages =
          lib.mapAttrs' (
            k: v: {
              name =
                if builtins.elem k ["tap" "default"]
                then k
                else "tap-${k}";
              value = pkgs.callPackage ./pkgTap.nix {rust-toolchain = v;};
            }
          )
          toolchains;
        # Different devshells for different rust versions
        devShells = lib.mapAttrs (_: v: mkDevShell v) toolchains;
      };
    };
}
