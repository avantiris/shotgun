{
  description = "shotgun - fast GPU video editor (and later DAW) for Linux/Wayland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # Native Access and Kontakt under Wine, and a yabridge built against the
  # same Wine (docs/spec/09-daw.md, "Kontakt"). It keeps its own nixpkgs:
  # its Wine is the one it was tested with.
  inputs.ni-wine.url = "github:selimbucher/native-instruments";

  outputs = { self, nixpkgs, ni-wine }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      # The same package set with yabridge built against ni-wine's Wine.
      bridged = import ni-wine.inputs.nixpkgs {
        inherit system;
        overlays = [ ni-wine.overlays.yabridge ];
      };
      niWine = ni-wine.packages.${system}.default;

      # Everything the program runs as a separate process. Nothing of it
      # has to be installed on the system (feedback F14.2): the packaged
      # program and the dev shell both bring it along.
      runtimeTools = with pkgs; [
        ffmpeg-full # ffmpeg, ffprobe: import, proxies, export
        yt-dlp # links
        gifski # GIF export
        git # project history
        wl-clipboard
        xdg-utils # "Show in folder"
        bash
        coreutils
      ] ++ musicTools;

      # Windows plugins: Native Access / Kontakt and the bridge to them.
      musicTools = [
        niWine # ni, native-access
        bridged.yabridge
        bridged.yabridgectl
      ];

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
      # The program with everything it needs: `nix run`, or put
      # `packages.x86_64-linux.default` into a system or home profile.
      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "shotgun";
        version = "0.1.0";
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          # Sources only: neither the documents nor the spikes change the build.
          filter = path: type:
            let rel = pkgs.lib.removePrefix (toString ./. + "/") (toString path);
            in !(pkgs.lib.hasPrefix "docs" rel || pkgs.lib.hasPrefix "spikes" rel || pkgs.lib.hasPrefix "scripts" rel);
        };
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "-p" "sg-app" ];
        # The tests want media, a GPU and a sound server; they run in the dev shell.
        doCheck = false;
        nativeBuildInputs = with pkgs; [ pkg-config clang mold makeWrapper ];
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
        ];
        LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        postFixup = ''
          wrapProgram $out/bin/shotgun \
            --prefix PATH : ${pkgs.lib.makeBinPath runtimeTools} \
            --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibs}
        '';
        meta.mainProgram = "shotgun";
      };

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
          git-lfs
          grim
          vulkan-tools
          libva-utils
          mediainfo
        ];

        packages = runtimeTools;

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
