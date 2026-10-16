{
  description = "Typst previews of LaTeX math in markdown, as a helix (steel) plugin";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    helix-plugins = {
      url = "git+https://codeberg.org/maxschipper/helix-plugins-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    helix-plugins,
  }: let
    systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];
    forAllSystems = f:
      nixpkgs.lib.genAttrs systems (system:
        f (import nixpkgs {
          inherit system;
          overlays = [helix-plugins.overlays.default];
        }));
  in {
    packages = forAllSystems (pkgs: let
      inherit (pkgs) lib;

      math-hx = pkgs.helixPlugins.buildHelixPluginWithNative {
        pname = "math-hx";
        version = "0.1.0";

        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./Cargo.toml
            ./Cargo.lock
            ./src
            ./assets
            (lib.fileset.fileFilter (file: file.hasExt "scm") ./.)
          ];
        };

        cargoHash = "sha256-x6wVcoKHL1L9qWosqGFdK7u/YMjUz/qyy4f7cljqGh0=";

        meta = {
          description = "Typst previews of LaTeX math in markdown";
          license = lib.licenses.mit;
        };
      };

      # Stock settings plus this plugin.
      configDir = pkgs.runCommand "math-hx-helix-config" {} ''
        mkdir -p $out/helix
        echo '(require "math-hx/math.scm")' > $out/helix/init.scm
        touch $out/helix/helix.scm
      '';

      # steelix reading its config from the store, so ~/.config/helix is never
      # used. STEEL_HOME has to be writable (steelix writes its builtin
      # `helix/*.scm` modules there on startup), so that one is a temp dir.
      hx = pkgs.writeShellApplication {
        name = "hx";
        text = ''
          steel_home=$(mktemp -d -t math-hx-steel.XXXXXX)
          trap 'rm -rf "$steel_home"' EXIT
          mkdir -p "$steel_home/cogs"
          ln -s ${math-hx} "$steel_home/cogs/math-hx"
          ln -s ${math-hx.native} "$steel_home/native"

          XDG_CONFIG_HOME=${configDir} \
            HELIX_STEEL_CONFIG=${configDir}/helix \
            STEEL_HOME="$steel_home" \
            ${lib.getExe' pkgs.steelix "hx"} "$@"
        '';
      };
    in {
      inherit math-hx hx;
      default = hx;
    });

    # `cargo test`, `cargo run` (regenerates math-hx.scm) and friends, with an
    # incremental target/ instead of a full `nix build` per change.
    devShells = forAllSystems (pkgs: {
      default = pkgs.mkShell {
        packages = with pkgs; [cargo rustc rustfmt clippy rust-analyzer];
        env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    });

    apps = forAllSystems (pkgs: {
      default = {
        type = "app";
        program = nixpkgs.lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.hx;
      };
    });
  };
}
