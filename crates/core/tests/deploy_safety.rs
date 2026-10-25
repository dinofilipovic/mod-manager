//! Deployment must never lose an original game file, whatever goes wrong.

mod fixtures;

use std::fs;

use mm_core::env::crossover;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::store::{JournalEntry, JournalOp};
use mm_core::{GameDefinition, GameInstance, scan_environments};

struct Setup {
    _tmp: tempfile::TempDir,
    lib: Library,
    game: GameDefinition,
    instance: GameInstance,
    archives: std::path::PathBuf,
}

fn setup() -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let bottle = fixtures::fake_bottle(tmp.path(), "Cyberpunk 2077");
    let game = find_builtin("cyberpunk2077").unwrap();
    let envs = crossover::discover_bottles(&bottle.bottles_dir).unwrap();
    let instance = scan_environments(std::slice::from_ref(&game), envs)
        .instances
        .remove(0);
    Setup {
        lib: Library::open(&tmp.path().join("data")).unwrap(),
        game,
        instance,
        archives: tmp.path().join("archives"),
        _tmp: tmp,
    }
}

/// A mod that replaces an original game file and adds files sorting before
/// and after it, so a failure can be placed after the backup was taken.
fn replacing_mod() -> fixtures::Entries {
    vec![
        ("archive/pc/mod/early.archive".into(), b"early".to_vec()),
        (
            "engine/config/base/general.ini".into(),
            b"[General]\nModded=1\n".to_vec(),
        ),
        ("r6/scripts/late/main.reds".into(), b"late".to_vec()),
    ]
}

#[test]
fn failed_deploy_keeps_original_backups() {
    let mut s = setup();
    let original = fixtures::snapshot(&s.instance.install_path);
    let archive = fixtures::write_zip(&s.archives.join("Replace.zip"), &replacing_mod());
    let id = s.instance.id.clone();
    let installed = s
        .lib
        .install_archive(&s.game, &archive, ModMeta::default(), Some(&id))
        .unwrap();

    // The last file can't be placed: its library copy is gone.
    let late = s
        .lib
        .root()
        .join("mods")
        .join(&installed.record.id)
        .join("r6/scripts/late/main.reds");
    fs::remove_file(&late).unwrap();
    assert!(s.lib.deploy(&s.game, &s.instance).is_err());

    // Put it back and deploy again, then purge everything.
    fs::write(&late, "late").unwrap();
    s.lib.deploy(&s.game, &s.instance).unwrap();
    assert_eq!(
        fs::read_to_string(
            s.instance
                .install_path
                .join("engine/config/base/general.ini")
        )
        .unwrap(),
        "[General]\nModded=1\n"
    );
    s.lib.purge(&s.game, &s.instance).unwrap();

    assert_eq!(
        fs::read_to_string(
            s.instance
                .install_path
                .join("engine/config/base/general.ini")
        )
        .unwrap(),
        "[General]\nOriginal=1\n",
        "the original game file is back"
    );
    assert_eq!(fixtures::snapshot(&s.instance.install_path), original);
}

#[test]
fn crash_mid_purge_is_finished_when_the_library_opens() {
    let mut s = setup();
    let original = fixtures::snapshot(&s.instance.install_path);
    let archive = fixtures::write_zip(&s.archives.join("Replace.zip"), &replacing_mod());
    let id = s.instance.id.clone();
    s.lib
        .install_archive(&s.game, &archive, ModMeta::default(), Some(&id))
        .unwrap();
    s.lib.deploy(&s.game, &s.instance).unwrap();

    // The app stopped while removing the mod's file: it is deleted, the
    // original isn't back yet and the step is still pending.
    let target = "engine/config/base/general.ini";
    fs::remove_file(s.instance.install_path.join(target)).unwrap();
    s.lib
        .store()
        .journal_begin(
            &JournalEntry {
                id: 0,
                instance_id: id.clone(),
                game_root: s.instance.install_path.to_string_lossy().into_owned(),
                op: JournalOp::Remove,
                target: target.into(),
                source: None,
                mod_id: None,
                revision: None,
                sha256: None,
                backup: None,
                moved_original: false,
            },
            0,
        )
        .unwrap();
    let root = s.lib.root().to_path_buf();
    drop(std::mem::replace(&mut s.lib, Library::open(&root).unwrap()));

    assert_eq!(
        fs::read_to_string(s.instance.install_path.join(target)).unwrap(),
        "[General]\nOriginal=1\n",
        "opening the library finished the removal"
    );
    assert!(s.lib.store().pending_journal().unwrap().is_empty());
    s.lib.purge(&s.game, &s.instance).unwrap();
    assert_eq!(fixtures::snapshot(&s.instance.install_path), original);
}

#[test]
fn a_stuck_recovery_elsewhere_doesnt_block_this_install() {
    let mut s = setup();
    // Another install's interrupted step can't be finished: its "game
    // folder" is a file, as if the drive held something else now.
    let elsewhere = s.archives.parent().unwrap().join("not-a-folder");
    fs::write(&elsewhere, "x").unwrap();
    let source = s.archives.parent().unwrap().join("source.reds");
    fs::write(&source, "x").unwrap();
    s.lib
        .store()
        .journal_begin(
            &JournalEntry {
                id: 0,
                instance_id: "cyberpunk2077-other".into(),
                game_root: elsewhere.to_string_lossy().into_owned(),
                op: JournalOp::Replace,
                target: "r6/scripts/x.reds".into(),
                source: Some(source.to_string_lossy().into_owned()),
                mod_id: Some("m".into()),
                revision: Some(1),
                sha256: None,
                backup: None,
                moved_original: false,
            },
            0,
        )
        .unwrap();
    let root = s.lib.root().to_path_buf();
    drop(std::mem::replace(&mut s.lib, Library::open(&root).unwrap()));
    assert_eq!(s.lib.warnings().len(), 1, "reported, not fatal");

    let archive = fixtures::write_zip(&s.archives.join("Replace.zip"), &replacing_mod());
    let id = s.instance.id.clone();
    s.lib
        .install_archive(&s.game, &archive, ModMeta::default(), Some(&id))
        .unwrap();
    s.lib.deploy(&s.game, &s.instance).unwrap();
    assert_eq!(
        s.lib.store().pending_journal().unwrap().len(),
        1,
        "still waiting"
    );
}
