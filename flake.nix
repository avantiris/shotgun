{
  description = "shotgun - fast GPU video editor (and later DAW) for Linux/Wayland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # Native Access and Kontakt under Wine, and a yabridge built against the
  # same Wine (docs/spec/09-daw.md, "Kontakt"). It uses this flake's
  # nixpkgs, not the older one it pins: its Wine then had an older C
  # library than the system's graphics drivers, could load none of them
  # (no OpenGL, no Vulkan), and Kontakt's window stopped at "Failed to
  # initialize graphics backend for D3D11" (docs/spikes/s7-plugin-host.md).
  inputs.ni-wine.url = "github:selimbucher/native-instruments";
  inputs.ni-wine.inputs.nixpkgs.follows = "nixpkgs";

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
        curl # fetches the matting model on its first use
        sgMatte # "Cut out the person"
      ] ++ musicTools;

      # Cutting a person out of a video (docs/spikes/s8-matte.md): Robust
      # Video Matting on onnxruntime, as a program of its own so that the
      # editor neither links the runtime nor waits on it.
      sgMatte = pkgs.writeShellScriptBin "sg-matte" ''
        exec ${pkgs.python3.withPackages (p: [ p.onnxruntime p.numpy ])}/bin/python3 -I ${./tools/sg-matte.py} "$@"
      '';

      # Fonts for titles that come with the program (feedback F17.1): one
      # folder of TrueType and OpenType files, named `Family-Style`, which
      # the program finds through SHOTGUN_FONTS. A font that is chosen is
      # copied into the project, so a project never depends on this folder.
      # Only fonts under a free licence; the build fails for any other.
      sgFonts =
        let
          # `"A"` takes the file A.ttf (or .otf) of the package under that
          # name; `"A=B"` takes it as B.
          picked = with pkgs; [
            # Plain
            { from = inter; files = [ "InterVariable=Inter-Regular" ]; }
            { from = roboto; files = [ "Roboto-Light" "Roboto-Regular" "Roboto-Italic" "Roboto-Bold" "Roboto-Black" "RobotoCondensed-Regular" "RobotoCondensed-Bold" ]; }
            { from = montserrat; files = [ "Montserrat-Light" "Montserrat-Regular" "Montserrat-Bold" "Montserrat-Black" ]; }
            { from = poppins; files = [ "Poppins-Light" "Poppins-Regular" "Poppins-Bold" "Poppins-Black" ]; }
            { from = lato; files = [ "Lato-Regular" "Lato-Bold" "Lato-Black" ]; }
            { from = open-sans; files = [ "OpenSans-Regular" "OpenSans-Bold" "OpenSans-ExtraBold" ]; }
            { from = raleway; files = [ "Raleway-Regular" "Raleway-Bold" "Raleway-Black" ]; }
            { from = work-sans; files = [ "WorkSans-Regular" "WorkSans-Bold" ]; }
            { from = rubik; files = [ "Rubik-Regular" "Rubik-Bold" "Rubik-Black" ]; }
            { from = quicksand; files = [ "Quicksand-Regular" "Quicksand-Bold" ]; }
            { from = barlow; files = [ "Barlow-Regular" "Barlow-Bold" "BarlowCondensed-Bold" "BarlowCondensed-Black" ]; }
            # For headlines
            { from = oswald; files = [ "Oswald-Regular" "Oswald-Bold" ]; }
            { from = league-gothic; files = [ "LeagueGothic-Regular" "LeagueGothic-Condensed" ]; }
            { from = league-spartan; files = [ "LeagueSpartan-Regular" "LeagueSpartan-Bold" "LeagueSpartan-Black" ]; }
            { from = the-neue-black; files = [ "TheNeue-Black" ]; }
            { from = norwester-font; files = [ "norwester=Norwester-Regular" ]; }
            { from = chunk; files = [ "ChunkFive-Regular" ]; }
            { from = orbitron; files = [ "Orbitron Bold=Orbitron-Bold" "Orbitron Black=Orbitron-Black" ]; }
            { from = knewave; files = [ "knewave=Knewave-Regular" ]; }
            # With serifs
            { from = eb-garamond; files = [ "EBGaramond12-Regular=EB Garamond-Regular" "EBGaramond12-Italic=EB Garamond-Italic" ]; }
            { from = libre-baskerville; files = [ "LibreBaskerville-Regular" "LibreBaskerville-Italic" "LibreBaskerville-Bold" ]; }
            { from = merriweather; files = [ "Merriweather-Regular" "Merriweather-Bold" "Merriweather-Black" ]; }
            { from = cormorant; files = [ "CormorantGaramond-Regular" "CormorantGaramond-Bold" ]; }
            { from = libre-bodoni; files = [ "LibreBodoni-Regular" "LibreBodoni-Bold" ]; }
            { from = zilla-slab; files = [ "ZillaSlab-Regular" "ZillaSlab-Bold" ]; }
            { from = roboto-slab; files = [ "RobotoSlab-Regular" "RobotoSlab-Bold" ]; }
            # Typewriter and screen
            { from = jetbrains-mono; files = [ "JetBrainsMono-Regular" "JetBrainsMono-Bold" ]; }
            { from = courier-prime; files = [ "CourierPrime-Regular" "CourierPrime-Bold" ]; }
            { from = vt323; files = [ "VT323-Regular" ]; }
            # By hand
            { from = dancing-script; files = [ "DancingScript-Regular" "DancingScript-Bold" ]; }
            { from = comic-neue; files = [ "ComicNeue-Regular" "ComicNeue-Bold" ]; }
            { from = shantell-sans; files = [ "Shantell_Sans-Normal-Regular=ShantellSans-Regular" "Shantell_Sans-Normal-Bold=ShantellSans-Bold" ]; }
          ];
          licences = l: map (one: one.spdxId or one.shortName) (pkgs.lib.toList l);
          take = { from, files }:
            assert pkgs.lib.all (one: one.free or false) (pkgs.lib.toList from.meta.license);
            pkgs.lib.concatMapStringsSep "\n"
              (file:
                let parts = pkgs.lib.splitString "=" file;
                in "take ${from} ${pkgs.lib.escapeShellArg (builtins.head parts)} ${pkgs.lib.escapeShellArg (pkgs.lib.last parts)}")
              files
            + "\necho ${pkgs.lib.escapeShellArg "${from.pname or from.name}: ${toString (licences from.meta.license)}, ${from.meta.homepage or ""}"} >> $out/LICENCES.txt";
        in
        pkgs.runCommand "shotgun-fonts" { } ''
          mkdir -p $out
          take() {
            # (TrueType before OpenType where a package has both.)
            found=$(find -L "$1" -type f \( -name "$2.ttf" -o -name "$2.otf" \) | sort -t. -k2 -r | head -1)
            if [ -z "$found" ]; then echo "no font file $2 in $1" >&2; exit 1; fi
            cp "$found" "$out/$3.''${found##*.}"
          }
          ${pkgs.lib.concatMapStringsSep "\n" take picked}
        '';

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
        # The program, and the process its plugins run in (found beside it).
        cargoBuildFlags = [ "-p" "sg-app" "-p" "sg-plug" ];
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
          # (What the library path was before: plugins are started with that one.)
          wrapProgram $out/bin/shotgun \
            --run 'export SHOTGUN_OUTER_LD_LIBRARY_PATH="''${LD_LIBRARY_PATH:-}"' \
            --set-default SHOTGUN_FONTS ${sgFonts} \
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
          # A native plugin (instrument and effect) for the plugin host's tests.
          export SHOTGUN_TEST_VST3=${pkgs.surge-xt}/lib/vst3
          # The fonts for titles that come with the program.
          export SHOTGUN_FONTS=''${SHOTGUN_FONTS:-${sgFonts}}
          # Plugins run with the library path the system has, not this shell's
          # (crates/sg-plug: `Client::start`). The shell replaces the path, so: none.
          export SHOTGUN_OUTER_LD_LIBRARY_PATH=
          mkdir -p "$CARGO_TARGET_DIR" "$SHOTGUN_DEV_DIR/media" 2>/dev/null || true
        '';
      };
    };
}
