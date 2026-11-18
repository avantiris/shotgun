{
  description = "shotgun - fast GPU video editor (and later DAW) for Linux/Wayland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };

      # Libraries that are dlopen()ed at runtime (winit, wgpu, libva) and
      # therefore must be on LD_LIBRARY_PATH on NixOS.
      runtimeLibs = with pkgs; [
        wayland
        libxkbcommon
        vulkan-loader
        libGL
        libva
        libdrm
        pipewire
        alsa-lib
      ];
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [
          # Rust toolchain
          cargo
          rustc
          clippy
          rustfmt
          rust-analyzer
          cargo-nextest
          # native build helpers (bindgen needs libclang, mold for fast links)
          pkg-config
          clang
          mold
          # external tools the app shells out to / we use for verification
          ffmpeg-full
          yt-dlp
          gifski
          git
          git-lfs
          wl-clipboard
          grim
          vulkan-tools
          libva-utils
          mediainfo
        ];

        buildInputs = with pkgs; [
          ffmpeg-full
          pipewire
          alsa-lib
          libva
          libdrm
          wayland
          libxkbcommon
          vulkan-loader
          vulkan-headers
          fontconfig
          freetype
        ] ++ runtimeLibs;

        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;

        shellHook = ''
          # The home disk is nearly full: keep all build output on the big drive.
          # An outer CARGO_TARGET_DIR wins, so spikes can use their own directory.
          export CARGO_TARGET_DIR=''${CARGO_TARGET_DIR:-/disk/shotgun-dev/target}
          export SHOTGUN_DEV_DIR=/disk/shotgun-dev
          export SHOTGUN_LIBRARY=''${SHOTGUN_LIBRARY:-/disk/shotgun}
          mkdir -p "$CARGO_TARGET_DIR" "$SHOTGUN_DEV_DIR/media" 2>/dev/null || true
        '';
      };
    };
}
