{
  description = "Kiln, a privacy-first control plane for coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      forAllSystems =
        f:
        nixpkgs.lib.genAttrs [
          "x86_64-linux"
          "aarch64-linux"
          "aarch64-darwin"
        ] (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (
        pkgs:
        let
          inherit (pkgs) lib stdenv;

          # GPUI loads these at runtime, so the linker cannot discover them.
          runtimeLibs = with pkgs; [
            libxkbcommon
            vulkan-loader
            wayland
          ];

          # macOS builds with Xcode's compiler and Metal toolchain.
          mkShell = if stdenv.hostPlatform.isDarwin then pkgs.mkShellNoCC else pkgs.mkShell;
        in
        {
          default = mkShell (
            {
              packages = with pkgs; [
                cargo
                clippy
                rust-analyzer
                rustc
                rustfmt
              ];

              RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
            }
            // lib.optionalAttrs stdenv.hostPlatform.isLinux {
              nativeBuildInputs = [ pkgs.pkg-config ];

              buildInputs = runtimeLibs ++ [ pkgs.libxcb ];

              # An rpath instead of LD_LIBRARY_PATH, which would leak into every
              # process Kiln spawns.
              "CARGO_TARGET_${stdenv.hostPlatform.rust.cargoEnvVarTarget}_RUSTFLAGS" =
                "-C link-arg=-Wl,-rpath,${lib.makeLibraryPath runtimeLibs}";
            }
            // lib.optionalAttrs stdenv.hostPlatform.isDarwin {
              # The Nix SDK variables would hide Xcode's `metal` from `xcrun`.
              shellHook = ''
                unset DEVELOPER_DIR SDKROOT
              '';
            }
          );
        }
      );
    };
}
