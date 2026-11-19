//! End-to-end behaviour of a bundle on disk: create, edit, close, reopen,
//! crash recovery and external changes.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use sg_core::{AssetId, Clip, ClipId, Document, Op, Record};
use sg_project::{Journal, create, open, scan};

fn insert(doc: &Document, id: u64, start: i64) -> Op {
    let seq = &doc.project().sequences[0];
    Op::InsertClip {
        seq: seq.id,
        track: seq.tracks[0].id,
        clip: Clip { id: ClipId(id), asset: AssetId(1), start, len: 10, src_in: 0 },
    }
}

fn journal_path(root: &Path) -> std::path::PathBuf {
    root.join(".shotgun/journal/journal.log")
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn create_makes_the_required_layout_and_a_git_repo() {
    let lib = tempfile::tempdir().unwrap();
    let created = create(lib.path(), "  Züri Fäscht  ").unwrap();
    assert_eq!(created.git_error, None);
    let root = &created.path;
    assert_eq!(root.file_name().unwrap(), "zueri-faescht");
    for dir in [
        "assets/video",
        "assets/index",
        "sequences",
        "scenarios",
        "looks",
        "exports",
        ".shotgun/journal",
        ".shotgun/cache/proxy",
        ".shotgun/trash",
    ] {
        assert!(root.join(dir).is_dir(), "{dir}");
    }
    assert!(root.join("sequences/main.sg").is_file());
    assert_eq!(git(root, &["log", "--format=%s"]).trim(), "Create project Züri Fäscht");
    let tracked = git(root, &["ls-files"]);
    assert_eq!(
        tracked.lines().collect::<Vec<_>>(),
        [".gitattributes", ".gitignore", "project.toml", "sequences/main.sg"]
    );
    assert_eq!(git(root, &["status", "--porcelain"]), "");

    // Same name again gets its own folder.
    let second = create(lib.path(), "Züri Fäscht").unwrap();
    assert_eq!(second.path.file_name().unwrap(), "zueri-faescht-2");
    assert_eq!(create(lib.path(), " ").unwrap().path.file_name().unwrap(), "untitled");
}

#[test]
fn library_lists_projects_most_recent_first() {
    let lib = tempfile::tempdir().unwrap();
    let a = create(lib.path(), "Alpha").unwrap().path;
    let b = create(lib.path(), "Beta").unwrap().path;
    std::fs::create_dir(lib.path().join("not-a-project")).unwrap();
    std::fs::create_dir(lib.path().join("broken")).unwrap();
    std::fs::write(lib.path().join("broken/project.toml"), "format = ").unwrap();
    std::fs::write(a.join(".shotgun/local.toml"), "last_opened = 100\n").unwrap();
    std::fs::write(b.join(".shotgun/local.toml"), "last_opened = 200\n").unwrap();
    let entries = scan(lib.path()).unwrap();
    assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Beta", "Alpha", "broken"]);
    assert!(entries[2].error.is_some());
    assert!(entries[0].id.is_some());
}

#[test]
fn edits_and_history_survive_close_and_reopen() {
    let lib = tempfile::tempdir().unwrap();
    let root = create(lib.path(), "Edit me").unwrap().path;
    let mut opened = open(&root, || {}).unwrap();
    assert_eq!(opened.document.project().name, "Edit me");
    for i in 0..3 {
        let op = insert(&opened.document, i + 1, i as i64 * 10);
        let record = opened.document.apply(op, None).unwrap();
        opened.writer.record(record, opened.document.project().clone());
    }
    let record = opened.document.undo().unwrap().unwrap();
    opened.writer.record(record, opened.document.project().clone());
    let expected = opened.document.clone();
    opened.writer.close().unwrap().join().unwrap();

    let sg = std::fs::read_to_string(root.join("sequences/main.sg")).unwrap();
    assert_eq!(sg.lines().filter(|l| l.starts_with("clip ")).count(), 2);
    assert!(!root.join(".shotgun/journal/pending").exists());

    let mut again = open(&root, || {}).unwrap();
    assert_eq!(again.document, expected);
    assert_eq!((again.recovered_edits, again.history_reset), (0, false));
    // The history is live: redo brings the third clip back, undo walks to the start.
    assert!(again.document.redo().unwrap().is_some());
    assert_eq!(again.document.project().clip_count(), 3);
    while again.document.undo().unwrap().is_some() {}
    assert_eq!(again.document.project().clip_count(), 0);
}

#[test]
fn edits_not_yet_saved_are_recovered_from_the_journal() {
    let lib = tempfile::tempdir().unwrap();
    let root = create(lib.path(), "Crash").unwrap().path;
    let opened = open(&root, || {}).unwrap();
    let mut doc = opened.document.clone();
    opened.writer.close().unwrap().join().unwrap();

    // What a killed app leaves behind: journal lines, untouched text files,
    // and a torn last line.
    let (mut journal, _) = Journal::open(&journal_path(&root)).unwrap();
    for i in 0..4 {
        let op = insert(&doc, i + 1, i as i64 * 10);
        journal.append(&doc.apply(op, None).unwrap()).unwrap();
    }
    journal.append(&doc.undo().unwrap().unwrap()).unwrap();
    drop(journal);
    let mut bytes = std::fs::read(journal_path(&root)).unwrap();
    bytes.extend_from_slice(b"{\"t\":1,\"r\":{\"do\":{\"op\"");
    std::fs::write(journal_path(&root), bytes).unwrap();

    let reopened = open(&root, || {}).unwrap();
    assert_eq!(reopened.recovered_edits, 5);
    assert_eq!(reopened.document, doc);
    assert_eq!(reopened.document.project().clip_count(), 3);
    reopened.writer.close().unwrap().join().unwrap();
    // The recovered state was written out on close.
    let sg = std::fs::read_to_string(root.join("sequences/main.sg")).unwrap();
    assert_eq!(sg.lines().filter(|l| l.starts_with("clip ")).count(), 3);
    assert_eq!(open(&root, || {}).unwrap().recovered_edits, 0);
}

#[test]
fn files_changed_outside_win_and_reset_history() {
    let lib = tempfile::tempdir().unwrap();
    let root = create(lib.path(), "Outside").unwrap().path;
    let mut opened = open(&root, || {}).unwrap();
    let record = opened.document.apply(Op::SetProjectName { name: "Renamed".into() }, None).unwrap();
    opened.writer.record(record, opened.document.project().clone());
    opened.writer.close().unwrap().join().unwrap();

    // e.g. `git checkout` of an older state, or an edit by hand.
    let toml = std::fs::read_to_string(root.join("project.toml")).unwrap();
    std::fs::write(root.join("project.toml"), toml.replace("Renamed", "By hand")).unwrap();

    let reopened = open(&root, || {}).unwrap();
    assert!(reopened.history_reset);
    assert_eq!(reopened.document.project().name, "By hand");
    assert!(!reopened.document.history().can_undo());
    reopened.writer.close().unwrap().join().unwrap();
    let backups = std::fs::read_dir(root.join(".shotgun/journal"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
        .count();
    assert_eq!(backups, 1, "the old journal is kept");
    assert!(!open(&root, || {}).unwrap().history_reset);
}

#[test]
fn a_bundle_without_its_local_folder_opens() {
    let lib = tempfile::tempdir().unwrap();
    let root = create(lib.path(), "Unzipped").unwrap().path;
    std::fs::remove_dir_all(root.join(".shotgun")).unwrap();
    let opened = open(&root, || {}).unwrap();
    assert_eq!(opened.document.project().name, "Unzipped");
    assert!(!opened.history_reset);
    assert!(root.join(".shotgun/cache/thumb").is_dir());
}

#[test]
fn records_are_journalled_before_the_debounced_save() {
    let lib = tempfile::tempdir().unwrap();
    let root = create(lib.path(), "Timing").unwrap().path;
    let mut opened = open(&root, || {}).unwrap();
    let record = opened.document.apply(Op::SetProjectName { name: "Now".into() }, None).unwrap();
    opened.writer.record(record.clone(), opened.document.project().clone());
    // The journal line is there within milliseconds; the text file is not
    // rewritten until the editor has been idle for two seconds.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::fs::metadata(journal_path(&root)).unwrap().len() == 0 {
        assert!(std::time::Instant::now() < deadline, "journal not written");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(std::fs::read_to_string(root.join("project.toml")).unwrap().contains("Timing"));
    let status = opened.writer.status().clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while status.saved.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        assert!(std::time::Instant::now() < deadline, "debounced save did not happen");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(std::fs::read_to_string(root.join("project.toml")).unwrap().contains("Now"));
    assert_eq!(status.error(), None);
    let _ = Record::Undo;
}

#[test]
fn opening_a_non_project_fails_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let err = open(dir.path(), || {}).err().unwrap().to_string();
    assert!(err.contains("is not a project"), "{err}");
}

#[test]
fn an_older_state_is_restored_in_a_project_without_scenarios_or_assets() {
    let dir = tempfile::tempdir().unwrap();
    let root = sg_project::create(dir.path(), "Back").unwrap().path;
    // A change to a tracked file, committed on top of the first state.
    let sequence = root.join("sequences/main.sg");
    let original = std::fs::read_to_string(&sequence).unwrap();
    std::fs::write(&sequence, original.replace("name \"Main\"", "name \"Changed\"")).unwrap();
    git(&root, &["add", "-A"]);
    git(&root, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "change"]);
    let first = git(&root, &["rev-list", "--max-parents=0", "HEAD"]);
    // No scenario and no asset was ever tracked here: git knows neither path.
    sg_project::restore(&root, first.trim(), "the beginning").unwrap();
    assert_eq!(std::fs::read_to_string(&sequence).unwrap(), original);
    assert!(git(&root, &["log", "--oneline"]).lines().next().unwrap().contains("Restore: the beginning"));
}
