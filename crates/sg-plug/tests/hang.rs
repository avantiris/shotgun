//! A plugin that stops answering (rather than crashing) must cost only its
//! own sound and a bounded wait. Stood in for by a host program that loads
//! and then does nothing. (Its own file: the host program is named through
//! the environment, which belongs to the whole test process.)

#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use sg_plug::{Client, Patience};

#[test]
fn a_plugin_that_stops_answering_is_given_up_on_and_ended() {
    let dir = tempfile::tempdir().unwrap();
    let host = dir.path().join("hung-host");
    // Answers the load (ok, an effect without a window, named "Hung"), then never again.
    std::fs::write(&host, "#!/bin/sh\nprintf '\\001\\000\\000\\004\\000\\000\\000Hung'\nexec sleep 600\n").unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: the only test of this file, so nothing else reads the environment meanwhile.
    unsafe { std::env::set_var("SHOTGUN_PLUGIN_HOST", &host) };
    let patience = Patience { slow: Duration::from_secs(5), block: Duration::from_millis(150) };
    let mut plugin = Client::start_with(std::path::Path::new("/nowhere/Hung.vst3"), patience).unwrap();
    assert_eq!(plugin.loaded.name, "Hung");
    let pid = plugin.pid();
    // A block larger than a pipe holds: neither the writing nor the waiting may hang.
    let mut block = vec![0.25f32; 2 * 48_000];
    let started = Instant::now();
    assert!(!plugin.process(&mut block, &[]));
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    // The sound is left as it was, the plugin counts as gone and its process is ended.
    assert!(block.iter().all(|s| *s == 0.25));
    assert!(plugin.is_dead());
    let started = Instant::now();
    assert!(!plugin.process(&mut block, &[]));
    assert!(started.elapsed() < Duration::from_millis(50));
    drop(plugin);
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}
