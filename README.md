<p align="center"><img src="assets/icon/shotgun.svg" width="128" alt="Shotgun"></p>

# Shotgun

A fast video editor and DAW in one, for Linux. Everything is drawn on the
graphics card; projects are plain folders under git.

Linux on x86-64 with Wayland, Vulkan and PipeWire.

## Get it

- **AppImage**: download `shotgun-<version>-x86_64.AppImage` from the
  [releases](https://github.com/avantiris/shotgun/releases/latest), make
  it executable, run it. For AMD and Intel graphics; with
  Nvidia's driver use Nix.
- **Nix**: `nix run github:avantiris/shotgun`, or add
  `packages.x86_64-linux.default` of this flake to your profile (launcher
  entry and icon included).

## Build

```
nix build              # the program, in ./result
nix build .#appimage   # the AppImage
nix develop            # a shell with the toolchain: cargo build, cargo nextest run
```

A push to `main` makes a release when its commits call for one
([conventional commits](https://www.conventionalcommits.org): `fix:` a
patch, `feat:` a minor, `feat!:` a major version).

## Licence

Copyright (C) 2026 Avantiris Association.

Free software under the GNU Affero General Public License, version 3: see
[LICENSE](LICENSE). It comes without any warranty.
