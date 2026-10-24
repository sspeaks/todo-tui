# Nix packaging for todo-tui: `nix build`, `nix run`, `nix profile add`,
# `nix develop`. The default package is `todo` (CLI + daemon) with the TUI
# bundle at share/todo-tui/tui.js and Node 24 on its PATH.
#
# Maintenance:
#   - pnpm-lock.yaml changed: update `pnpmDepsHash` below. Set it to "" and run
#       nix build .#todo-tui-js
#     which fails with `got: sha256-...`; paste that value in. (A stale hash
#     can show up as ERR_PNPM_NO_OFFLINE_TARBALL instead of a hash mismatch.)
#   - Cargo.lock changed: nothing, unless a git dependency appears; then add
#     its hash under `cargoLock.outputHashes`.
#   - rust-toolchain.toml raised past nixpkgs' rustc: `nix flake update`.
#   - The build reads a new top-level file or directory: add it to `rustSrc`
#     or `jsSrc`. Nix also sees only git-tracked files, so `git add` first.
#   - Something new to install (say the MCP server): add a package for it and
#     copy it into `default`.
# aarch64-darwin evaluates but has not been built or run.
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
      pnpmDepsHash = "sha256-UQCNT9d14RUBXuWQ1/wT2zJnQER+uWKAedS0yi2KLGk=";

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
              hash = pnpmDepsHash;
            };
            buildPhase = ''
              runHook preBuild
              pnpm gen
              pnpm -C tui build
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              install -Dm644 tui/dist/tui.js $out/share/todo-tui/tui.js
              runHook postInstall
            '';
          });

          # `todo` runs <its own dir>/../share/todo-tui/tui.js with `node` from
          # PATH. The binary is copied, not symlinked, because the kernel
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
