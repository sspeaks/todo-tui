# Nix packaging for todo-tui: `nix build`, `nix run`, `nix profile add`,
# `nix develop`. The default package is `todo` (CLI + daemon) with the TUI
# and MCP server bundles at share/todo-tui/{tui,mcp}.js and Node 24 on its
# PATH.
#
# Maintenance:
#   - pnpm-lock.yaml changed: update every entry of `pnpmDepsHash` below. The
#     fetched store differs by platform (Linux and macOS get different hashes),
#     so each system has its own. The `nix` CI job builds each one and fails
#     with `got: sha256-...` for a stale entry; paste that value in. Locally,
#     set this system's entry to "" and run `nix build .#todo-tui-js`. (A stale
#     hash can show up as ERR_PNPM_NO_OFFLINE_TARBALL instead of a mismatch.)
#   - Cargo.lock changed: nothing, unless a git dependency appears; then add
#     its hash under `cargoLock.outputHashes`.
#   - rust-toolchain.toml raised past nixpkgs' rustc: `nix flake update`.
#   - The build reads a new top-level file or directory: add it to `rustSrc`
#     or `jsSrc`. Nix also sees only git-tracked files, so `git add` first.
#   - Something new to install: build it in `todo-tui-js` (or a package of
#     its own) and copy it into `default`.
{
  description = "todo-tui: a local-first todo list with a daemon, a CLI and an Ink TUI";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      version = (lib.importTOML ./Cargo.toml).workspace.package.version;
      # Per system: see the maintenance notes above.
      pnpmDepsHash = {
        x86_64-linux = "sha256-J9fU109O0t2DdPwzzehAWrqnIPBdM98Uh2uhAGqMK7k=";
        aarch64-linux = "sha256-J9fU109O0t2DdPwzzehAWrqnIPBdM98Uh2uhAGqMK7k=";
        aarch64-darwin = "sha256-GD0k69V28HOBnQrUXKOfaYzyMW3rS9mVsZlZV2q1HRA=";
      };

      # Each part sees only the files it builds from, so a README edit
      # rebuilds nothing and a TUI edit does not rebuild the Rust binary.
      src =
        files:
        lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions files;
        };
      rustSrc = src [
        ./Cargo.toml
        ./Cargo.lock
        ./crates
        ./proto
      ];
      jsSrc = src [
        ./package.json
        ./pnpm-lock.yaml
        ./pnpm-workspace.yaml
        ./tsconfig.base.json
        ./buf.yaml
        ./buf.gen.yaml
        ./proto
        ./packages
        ./tui
        ./mcp
      ];

      packagesFor =
        pkgs:
        let
          nodejs = pkgs.nodejs_24;
          pnpm = pkgs.pnpm_12;
        in
        rec {
          todo-cli = pkgs.rustPlatform.buildRustPackage {
            pname = "todo-cli";
            inherit version;
            src = rustSrc;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "--package=todo-cli" ];
            nativeBuildInputs = [ pkgs.protobuf ];
            # The integration tests spawn daemons on Unix sockets, which the
            # build sandbox does not allow; CI runs them.
            doCheck = false;
            meta.mainProgram = "todo";
          };

          todo-tui-js = pkgs.stdenvNoCC.mkDerivation (finalAttrs: {
            pname = "todo-tui-js";
            inherit version;
            src = jsSrc;
            nativeBuildInputs = [
              nodejs
              pnpm
              pkgs.pnpmConfigHook
            ];
            pnpmDeps = pkgs.fetchPnpmDeps {
              inherit (finalAttrs) pname version src;
              inherit pnpm;
              fetcherVersion = 4;
              hash = pnpmDepsHash.${pkgs.stdenv.hostPlatform.system};
            };
            buildPhase = ''
              runHook preBuild
              pnpm gen
              pnpm -C tui build
              pnpm -C mcp build
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              install -Dm644 tui/dist/tui.js $out/share/todo-tui/tui.js
              install -Dm644 mcp/dist/mcp.js $out/share/todo-tui/mcp.js
              runHook postInstall
            '';
          });

          # `todo` runs <its own dir>/../share/todo-tui/tui.js (`todo mcp`,
          # mcp.js) with `node` from PATH. The binary is copied, not symlinked, because the kernel
          # reports a symlinked exe by its target; the wrapper adds Node 24.
          default =
            pkgs.runCommand "todo-tui-${version}"
              {
                nativeBuildInputs = [ pkgs.makeWrapper ];
                meta.mainProgram = "todo";
              }
              ''
                install -Dm755 ${todo-cli}/bin/todo $out/bin/todo
                install -Dm644 ${todo-tui-js}/share/todo-tui/tui.js $out/share/todo-tui/tui.js
                install -Dm644 ${todo-tui-js}/share/todo-tui/mcp.js $out/share/todo-tui/mcp.js
                wrapProgram $out/bin/todo --prefix PATH : ${lib.makeBinPath [ nodejs ]}
              '';
        };
    in
    {
      packages = forAllSystems packagesFor;

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = lib.getExe self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          meta.description = "todo: the TUI with no arguments, else the CLI";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.protobuf
            pkgs.nodejs_24
            pkgs.pnpm_12
            pkgs.buf
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
