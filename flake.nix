{
  description = "nub — fast TypeScript-first runtime and pnpm-compatible package manager for Node";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      version = "0.2.2";

      # Per-system prebuilt release tarball + its sha256, verified against the
      # GitHub release assets for v${version}. nub ships prebuilt per-platform
      # tarballs on every channel (curl installer, Homebrew, npm); this flake lays
      # one out rather than building from source. WHY prebuilt, not buildRustPackage:
      # a from-source cargo build omits the vendored runtime/ tree (preload +
      # node_modules + the nub-native.node N-API addon) that nub resolves beside its
      # binary at run time — a bare cargo binary passes `--version` but fails real
      # TypeScript workloads. The prebuilt tarball already carries that layout.
      assets = {
        "x86_64-linux" = {
          file = "nub-linux-x64.tar.gz";
          sha256 = "6cc63a89f25f12719bce9afc97e513cc8ee22ef203e4a72a3c7398e62b413a23";
        };
        "aarch64-linux" = {
          file = "nub-linux-arm64.tar.gz";
          sha256 = "b9a292a725a959809fd629e7b3d8d6d886480300b8451bb41f8fb4a5098107ec";
        };
        "x86_64-darwin" = {
          file = "nub-darwin-x64.tar.gz";
          sha256 = "39c0f5200be3688e776c51ee2978e3cfe50fdb50946261a52ca42f6481145d75";
        };
        "aarch64-darwin" = {
          file = "nub-darwin-arm64.tar.gz";
          sha256 = "1c561d820145e9eb7640f6f97c0fe2f2d8b8d4a4d64b19f78fccf8f9dd79ac46";
        };
      };

      systems = builtins.attrNames assets;
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);

      nubFor =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          asset = assets.${system};
        in
        pkgs.stdenv.mkDerivation {
          pname = "nub";
          inherit version;

          src = pkgs.fetchurl {
            url = "https://github.com/nubjs/nub/releases/download/v${version}/${asset.file}";
            sha256 = asset.sha256;
          };

          # The tarball expands to bin/ + runtime/ at the top level (no wrapping dir).
          sourceRoot = ".";

          # The prebuilt Linux binary and nub-native.node link glibc (libc, libm,
          # libgcc_s) and hard-code a /lib64 interpreter — autoPatchelfHook rewrites
          # both for the Nix store. Darwin mach-o binaries need no patching.
          nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib ];

          dontConfigure = true;
          dontBuild = true;

          # Lay bin/nub (+ bin/nubx) and the runtime/ sibling out exactly as the
          # tarball ships them. nub canonicalizes current_exe() and walks UP from the
          # binary's directory to find runtime/preload.mjs, so the binary MUST be a
          # real file with runtime/ as a real sibling — no makeWrapper, no symlinked
          # entrypoint, which would canonicalize to a different directory and lose the
          # runtime/ tree.
          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            cp -r bin "$out/bin"
            cp -r runtime "$out/runtime"
            chmod +x "$out/bin/nub" "$out/bin/nubx"
            runHook postInstall
          '';

          # nub provisions and locates Node itself at run time (from PATH or its own
          # cache), so the package declares no Node runtime dependency here.

          meta = with pkgs.lib; {
            description = "Fast TypeScript-first runtime and pnpm-compatible package manager for Node";
            homepage = "https://nubjs.com";
            downloadPage = "https://github.com/nubjs/nub/releases";
            license = licenses.mit;
            mainProgram = "nub";
            platforms = systems;
            sourceProvenance = [ sourceTypes.binaryNativeCode ];
          };
        };
    in
    {
      packages = forAllSystems (
        system: rec {
          nub = nubFor system;
          default = nub;
        }
      );

      apps = forAllSystems (
        system:
        let
          nubPkg = nubFor system;
        in
        {
          nub = {
            type = "app";
            program = "${nubPkg}/bin/nub";
          };
          nubx = {
            type = "app";
            program = "${nubPkg}/bin/nubx";
          };
          default = {
            type = "app";
            program = "${nubPkg}/bin/nub";
          };
        }
      );
    };
}
