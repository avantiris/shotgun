//! Plugins on this machine: which are installed, and the set-up of the
//! Native Instruments ones (Native Access, Kontakt) from inside the
//! program (docs/spec/09-daw.md, "Kontakt"; feedback F14.3).
//!
//! Windows plugins run under Wine through yabridge, which makes each
//! appear as a native VST3 plugin. The tools (`ni`, `native-access`,
//! `yabridgectl`, Wine) come with Shotgun's own package (flake.nix);
//! nothing of Native Instruments' is shipped: the user logs in to their
//! own account in Native Access and installs from there.
//!
//! All calls here touch the disk or start programs: job threads only.

use std::path::{Path, PathBuf};
use std::process::Command;

use sg_media::tool::{Cancel, ToolError, run_lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Vst3,
    Clap,
}

/// A plugin found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    /// The bundle (`.vst3` folder) or file (`.clap`).
    pub path: PathBuf,
    pub format: Format,
    /// A Windows plugin behind the bridge.
    pub bridged: bool,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from)
}

/// The folders plugins are looked for in, in the order of the formats'
/// own conventions: the user's, then the system's and the Nix profiles'.
pub fn folders() -> Vec<(Format, PathBuf)> {
    let mut found = Vec::new();
    let user = std::env::var("USER").unwrap_or_default();
    for (format, dir, variable) in [(Format::Vst3, "vst3", "VST3_PATH"), (Format::Clap, "clap", "CLAP_PATH")] {
        if let Some(extra) = std::env::var_os(variable) {
            found.extend(std::env::split_paths(&extra).map(|p| (format, p)));
        }
        found.push((format, home().join(format!(".{dir}"))));
        for base in ["/usr/lib", "/usr/local/lib", "/run/current-system/sw/lib"] {
            found.push((format, Path::new(base).join(dir)));
        }
        found.push((format, home().join(".nix-profile/lib").join(dir)));
        found.push((format, PathBuf::from(format!("/etc/profiles/per-user/{user}/lib/{dir}"))));
    }
    found
}

/// Every plugin in `folders`, by name. Folders are searched three levels
/// deep (vendors keep theirs in subfolders; the bridge in `yabridge/`).
pub fn scan(folders: &[(Format, PathBuf)]) -> Vec<Plugin> {
    fn walk(format: Format, dir: &Path, depth: usize, out: &mut Vec<Plugin>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let wanted = match format {
                Format::Vst3 => "vst3",
                Format::Clap => "clap",
            };
            if path.extension().is_some_and(|e| e == wanted) {
                // A Windows plugin copied here by hand is not loadable as it is.
                let loadable = match format {
                    Format::Vst3 => path.is_dir(),
                    Format::Clap => path.is_file(),
                };
                if loadable {
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let bridged = path.components().any(|c| c.as_os_str() == "yabridge");
                    out.push(Plugin { name, path, format, bridged });
                }
            } else if depth > 0 && path.is_dir() {
                walk(format, &path, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    for (format, dir) in folders {
        walk(*format, dir, 3, &mut out);
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.path.cmp(&b.path)));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// Where the Wine prefix of the Native Instruments software is: where
/// `NI_WINE_PREFIX` says, else `~/.wine-ni` if it exists (set up outside
/// Shotgun), else a folder in the library, which is on the large disk
/// (the sound libraries are many gigabytes).
pub fn ni_prefix(library: &Path) -> PathBuf {
    if let Some(set) = std::env::var_os("NI_WINE_PREFIX") {
        return PathBuf::from(set);
    }
    let usual = home().join(".wine-ni");
    if usual.is_dir() { usual } else { library.join("_wine-ni") }
}

/// How far the set-up of the Native Instruments software has come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NiState {
    /// The programs that do the work were found (`ni`, `yabridgectl`).
    pub tools: bool,
    pub prefix: PathBuf,
    /// The Wine prefix exists.
    pub prefix_ready: bool,
    /// Native Access is installed in it.
    pub access: bool,
    /// Plugins installed through Native Access (their names).
    pub installed: Vec<String>,
    /// Those of them the bridge already offers.
    pub bridged: Vec<String>,
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The folder holding `bin/yabridge-host.exe` and `lib/libyabridge-*.so`
/// of the bridge on the `PATH` (the same search as `sg_plug::bridge_root`,
/// which the plugin's own process uses when it loads a bridged plugin).
fn bridge_root() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| dir.join("yabridge-host.exe").is_file())
        .filter_map(|dir| dir.parent().map(Path::to_path_buf))
        .find(|root| root.join("lib/libyabridge-vst3.so").is_file())
}

/// The folder Windows VST3 plugins are installed to, in a prefix.
fn windows_vst3(prefix: &Path) -> PathBuf {
    prefix.join("drive_c/Program Files/Common Files/VST3")
}

fn has_file(dir: &Path, name: &str, depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().any(|entry| {
        let path = entry.path();
        entry.file_name() == name || (depth > 0 && path.is_dir() && has_file(&path, name, depth - 1))
    })
}

/// Looks at the disk: nothing is started.
pub fn ni_state(library: &Path) -> NiState {
    let prefix = ni_prefix(library);
    let names = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "vst3"))
            .filter_map(|e| Some(e.path().file_stem()?.to_string_lossy().into_owned()))
            .collect();
        names.sort();
        names
    };
    let installed = names(&windows_vst3(&prefix));
    let offered = names(&home().join(".vst3/yabridge"));
    NiState {
        tools: on_path("ni") && on_path("yabridgectl"),
        prefix_ready: prefix.join("drive_c").is_dir(),
        access: has_file(&prefix.join("drive_c/Program Files/Native Instruments"), "Native Access.exe", 3),
        bridged: installed.iter().filter(|name| offered.contains(name)).cloned().collect(),
        installed,
        prefix,
    }
}

fn ni(prefix: &Path) -> Command {
    let mut command = Command::new("ni");
    command.env("NI_WINE_PREFIX", prefix);
    command
}

/// Step 1: creates the Wine prefix and installs Native Access into it.
/// Takes minutes; every line the tool prints goes to `on_line`.
pub fn ni_setup(prefix: &Path, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<(), ToolError> {
    if let Some(parent) = prefix.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut command = ni(prefix);
    command.args(["setup", "--no-ui"]);
    run_lines(command, cancel, |line| on_line(line))
}

/// Step 2: opens Native Access, where the user logs in and installs
/// their products. Returns once it has been closed.
pub fn ni_launch(prefix: &Path, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<(), ToolError> {
    let mut command = ni(prefix);
    command.arg("launch");
    run_lines(command, cancel, |line| on_line(line))
}

/// Has the tool look for what is wrong with the set-up (its
/// dependencies, the prefix, the login link from the browser) and repair
/// what it can; what it finds goes to `on_line`.
pub fn ni_check(prefix: &Path, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<(), ToolError> {
    let mut command = ni(prefix);
    command.args(["doctor", "--fix"]);
    run_lines(command, cancel, |line| on_line(line))
}

/// Where everything the steps print is kept, for when one of them fails.
pub fn ni_log(library: &Path) -> PathBuf {
    library.join("native-instruments-setup.log")
}

/// Whether the bridged plugins were set up by another build of the bridge
/// than the one that is here now (the program was updated since): each
/// bridged plugin is a copy of the bridge's loader, and an old loader
/// does not start with a new bridge. [`bridge`] sets them up anew.
pub fn bridge_stale() -> bool {
    let Some(loader) = bridge_root().map(|root| root.join("lib/libyabridge-chainloader-vst3.so")) else { return false };
    let Ok(current) = std::fs::read(loader) else { return false };
    let Ok(bundles) = std::fs::read_dir(home().join(".vst3/yabridge")) else { return false };
    bundles.flatten().any(|bundle| {
        let inside = bundle.path().join("Contents/x86_64-linux");
        let Ok(files) = std::fs::read_dir(inside) else { return false };
        files.flatten().filter(|f| f.path().extension().is_some_and(|x| x == "so")).any(|f| {
            // (Compared by content: the loader is small, and there are few plugins.)
            std::fs::read(f.path()).is_ok_and(|copy| copy != current)
        })
    })
}

/// Step 3: tells the bridge about the plugins installed in the prefix,
/// after which they are found by [`scan`] like native ones.
pub fn bridge(prefix: &Path, cancel: &Cancel, mut on_line: impl FnMut(&str)) -> Result<(), ToolError> {
    let folder = windows_vst3(prefix);
    if !folder.is_dir() {
        return Err(ToolError::other("no plugins are installed yet: install them in Native Access first"));
    }
    let tool = |arg: &str| {
        let mut command = Command::new("yabridgectl");
        command.arg(arg).env("WINEPREFIX", prefix);
        // The bridge that came with the program, not one a system profile
        // happens to hold (built against another Wine), and one at all
        // where the system has none: yabridgectl looks in these folders.
        if let Some(root) = bridge_root() {
            command.env("NIX_PROFILES", root);
        }
        command
    };
    let mut add = tool("add");
    add.arg(&folder);
    run_lines(add, cancel, |line| on_line(line))?;
    run_lines(tool("sync"), cancel, |line| on_line(line))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn plugins_are_found_in_their_folders_and_the_bridged_ones_marked() {
        let temp = tempfile::tempdir().unwrap();
        let (vst3, clap) = (temp.path().join("vst3"), temp.path().join("clap"));
        for bundle in ["Surge XT.vst3", "yabridge/Kontakt 8.vst3", "Vendor/Deep/Reverb.vst3"] {
            std::fs::create_dir_all(vst3.join(bundle).join("Contents")).unwrap();
        }
        // A Windows plugin file copied by hand, and something else: not offered.
        std::fs::write(vst3.join("OTT_x64.dll"), b"").unwrap();
        std::fs::write(vst3.join("Loose.vst3"), b"").unwrap();
        std::fs::create_dir_all(&clap).unwrap();
        std::fs::write(clap.join("Vital.clap"), b"").unwrap();
        let found =
            scan(&[(Format::Vst3, vst3.clone()), (Format::Clap, clap), (Format::Clap, temp.path().join("none"))]);
        let listed: Vec<(&str, Format, bool)> = found.iter().map(|p| (p.name.as_str(), p.format, p.bridged)).collect();
        assert_eq!(
            listed,
            [
                ("Kontakt 8", Format::Vst3, true),
                ("Reverb", Format::Vst3, false),
                ("Surge XT", Format::Vst3, false),
                ("Vital", Format::Clap, false),
            ]
        );
        assert_eq!(found[0].path, vst3.join("yabridge/Kontakt 8.vst3"));
    }

    #[test]
    fn the_state_of_the_set_up_is_read_from_the_prefix() {
        let temp = tempfile::tempdir().unwrap();
        // (The prefix is named directly: the environment belongs to the whole test run.)
        let prefix = temp.path().join("_wine-ni");
        assert!(!prefix.join("drive_c").is_dir());
        assert!(!has_file(&prefix.join("drive_c/Program Files/Native Instruments"), "Native Access.exe", 3));
        let access = prefix.join("drive_c/Program Files/Native Instruments/Native Access");
        std::fs::create_dir_all(&access).unwrap();
        std::fs::write(access.join("Native Access.exe"), b"").unwrap();
        assert!(has_file(&prefix.join("drive_c/Program Files/Native Instruments"), "Native Access.exe", 3));
        std::fs::create_dir_all(windows_vst3(&prefix).join("Kontakt 8.vst3")).unwrap();
        assert!(windows_vst3(&prefix).join("Kontakt 8.vst3").is_dir());
        // Nothing installed: bridging says what to do first.
        let empty = temp.path().join("empty");
        let said = bridge(&empty, &Cancel::new(), |_| {}).unwrap_err().to_string();
        assert!(said.contains("Native Access"), "{said}");
    }
}
