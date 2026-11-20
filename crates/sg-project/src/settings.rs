//! What belongs to this machine and not to a project: which input sound is
//! recorded from. Kept in `settings.toml` in the library (the scan for
//! projects only looks at folders). File I/O: job threads only.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// The PipeWire node sound is recorded from; `None`: the system's default input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input: Option<String>,
}

fn file(library: &Path) -> PathBuf {
    library.join("settings.toml")
}

/// The settings of this library; the defaults when there are none or they cannot be read.
pub fn load(library: &Path) -> Settings {
    std::fs::read_to_string(file(library)).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
}

pub fn save(library: &Path, settings: &Settings) -> io::Result<()> {
    let text = toml::to_string(settings).map_err(io::Error::other)?;
    // Written beside and renamed: a crash never leaves half a file.
    let temporary = library.join("settings.toml.new");
    std::fs::write(&temporary, text)?;
    std::fs::rename(temporary, file(library))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip_and_a_missing_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Settings::default());
        let settings = Settings { audio_input: Some("alsa_input.usb".to_owned()) };
        save(dir.path(), &settings).unwrap();
        assert_eq!(load(dir.path()), settings);
    }
}
