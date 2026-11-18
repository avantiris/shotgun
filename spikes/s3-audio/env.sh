# source inside `nix develop`: build/run environment for spike S3
export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s3
S3_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export PW_JACK_LIB=/nix/store/7y28iwgfcz2n3gwfl0c6xrb7vlg854nb-pipewire-1.6.9-jack/lib
export PKG_CONFIG_PATH="$S3_DIR/pkgconfig:$PKG_CONFIG_PATH"
# libjack.so.0 (PipeWire's) for the cpal JACK host; not on the system path on this machine
export LD_LIBRARY_PATH="$PW_JACK_LIB:$LD_LIBRARY_PATH"
