//! The whole mod lifecycle against a fake bottle: install, enable, deploy,
//! verify, update, purge. Whatever happens in between, purging leaves the
//! game folder byte-identical to how it started.

mod fixtures;

use std::fs;
use std::path::Path;

use mm_core::env::crossover;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::store::{JournalEntry, JournalOp};
use mm_core::{GameDefinition, GameInstance, scan_environments};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Trouble {
    None,
    /// A file can't be placed halfway through the update's deploy.
    FailedDeploy,
    /// The app stops while the update's deploy has a step half done.
    Crash,
}

struct World {
    tmp: tempfile::TempDir,
    game: GameDefinition,
    instance: GameInstance,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let bottle = fixtures::fake_bottle(tmp.path(), "Cyberpunk 2077");
    let game = find_builtin("cyberpunk2077").unwrap();
    let envs = crossover::discover_bottles(&bottle.bottles_dir).unwrap();
    let instance = scan_environments(std::slice::from_ref(&game), envs)
        .instances
        .remove(0);
    World {
        tmp,
        game,
        instance,
    }
}

fn mod_version(version: &str) -> fixtures::Entries {
    vec![
        (
            "archive/pc/mod/lifecycle.archive".into(),
            format!("RDAR lifecycle {version}").into_bytes(),
        ),
        (
            "engine/config/base/general.ini".into(),
            format!("[General]\nModded={version}\n").into_bytes(),
        ),
        (
            "r6/scripts/Lifecycle/main.reds".into(),
            format!("// {version}").into_bytes(),
        ),
        (
            format!("r6/scripts/Lifecycle/only_in_{version}.reds"),
            b"// extra".to_vec(),
        ),
    ]
}

/// Folders created by deploying that purging leaves on purpose: the ones
/// mods are dropped into, so manual installs keep working.
fn allowed_new_dirs(game: &GameDefinition) -> Vec<String> {
    let mut dirs = Vec::new();
    for t in game
        .layout
        .iter()
        .filter(|t| !t.extensions.is_empty() || t.marker_file.is_some())
    {
        let mut path = String::new();
        for part in t.path.split('/') {
            path = if path.is_empty() {
                part.to_string()
            } else {
                format!("{path}/{part}")
            };
            dirs.push(path.clone());
        }
    }
    dirs
}

fn assert_restored(
    w: &World,
    files: &std::collections::BTreeMap<String, Vec<u8>>,
    dirs: &[String],
) {
    let root = &w.instance.install_path;
    assert_eq!(
        &fixtures::snapshot(root),
        files,
        "byte-identical game folder"
    );
    let allowed = allowed_new_dirs(&w.game);
    let extra: Vec<String> = fixtures::dir_snapshot(root)
        .into_iter()
        .filter(|d| !dirs.contains(d) && !allowed.contains(d))
        .collect();
    assert!(extra.is_empty(), "folders left behind: {extra:?}");
}

fn lifecycle(trouble: Trouble) {
    let w = world();
    let root = w.instance.install_path.clone();
    let files_before = fixtures::snapshot(&root);
    let dirs_before = fixtures::dir_snapshot(&root);
    let data = w.tmp.path().join("data");
    let mut lib = Library::open(&data).unwrap();
    let id = w.instance.id.clone();

    // Install and enable version 1, then deploy it.
    let v1 = fixtures::write_zip(
        &w.tmp.path().join("in/Lifecycle-77-1-0-1700000000.zip"),
        &mod_version("1"),
    );
    let installed = lib
        .install_archive(&w.game, &v1, ModMeta::default(), Some(&id))
        .unwrap();
    let mod_id = installed.record.id.clone();
    lib.set_mod_enabled(&w.game, &id, &mod_id, false).unwrap();
    assert!(!lib.loadout(&w.game, &w.instance).unwrap().mods[0].enabled);
    lib.set_mod_enabled(&w.game, &id, &mod_id, true).unwrap();
    let report = lib.deploy(&w.game, &w.instance).unwrap();
    assert_eq!((report.added, report.backed_up), (4, 1));
    assert_eq!(
        fs::read_to_string(root.join("engine/config/base/general.ini")).unwrap(),
        "[General]\nModded=1\n"
    );
    let check = lib.verify_deployment(&w.game, &w.instance).unwrap();
    assert_eq!((check.checked, check.drifted.len()), (4, 0));
    assert!(!lib.loadout(&w.game, &w.instance).unwrap().pending_changes);

    // Update to version 2.
    let v2 = fixtures::write_7z(
        &w.tmp.path().join("in/Lifecycle-77-2-0-1700000001.7z"),
        &mod_version("2"),
    );
    let meta = ModMeta {
        replaces: Some(mod_id.clone()),
        ..Default::default()
    };
    let updated = lib.install_archive(&w.game, &v2, meta, Some(&id)).unwrap();
    assert!(updated.replaced_existing);
    assert_eq!(updated.record.revision, 2);
    assert!(lib.loadout(&w.game, &w.instance).unwrap().pending_changes);

    match trouble {
        Trouble::None => {}
        Trouble::FailedDeploy => {
            // The updated script can't be read from the library.
            let source = data
                .join("mods")
                .join(&mod_id)
                .join("r6/scripts/Lifecycle/only_in_2.reds");
            let saved = fs::read(&source).unwrap();
            fs::remove_file(&source).unwrap();
            let err = lib.deploy(&w.game, &w.instance).unwrap_err().to_string();
            assert!(err.contains("only_in_2.reds"), "{err}");
            assert!(lib.store().pending_journal().unwrap().is_empty());
            fs::write(&source, saved).unwrap();
        }
        Trouble::Crash => {
            // Half of an update deploy happened: the old script was taken
            // away, the new one placed but not recorded, and the app stopped.
            let new_target = "r6/scripts/Lifecycle/only_in_2.reds";
            fs::copy(
                data.join("mods").join(&mod_id).join(new_target),
                root.join(new_target),
            )
            .unwrap();
            lib.store()
                .journal_begin(
                    &JournalEntry {
                        id: 0,
                        instance_id: id.clone(),
                        game_root: root.to_string_lossy().into_owned(),
                        op: JournalOp::Add,
                        target: new_target.into(),
                        source: Some(
                            data.join("mods")
                                .join(&mod_id)
                                .join(new_target)
                                .to_string_lossy()
                                .into_owned(),
                        ),
                        mod_id: Some(mod_id.clone()),
                        revision: Some(2),
                        sha256: None,
                        backup: None,
                        moved_original: false,
                    },
                    0,
                )
                .unwrap();
            drop(lib);
            lib = Library::open(&data).unwrap();
            assert!(
                !root.join(new_target).exists(),
                "the unrecorded file was taken away again"
            );
        }
    }

    let report = lib.deploy(&w.game, &w.instance).unwrap();
    match trouble {
        // The failed attempt recorded what it finished; the retry continues.
        Trouble::FailedDeploy => assert_eq!((report.added, report.unchanged), (1, 3), "{report:?}"),
        _ => assert_eq!(
            (report.replaced, report.added, report.removed),
            (3, 1, 1),
            "{report:?}"
        ),
    }
    assert_eq!(
        fs::read_to_string(root.join("engine/config/base/general.ini")).unwrap(),
        "[General]\nModded=2\n"
    );
    assert!(!root.join("r6/scripts/Lifecycle/only_in_1.reds").exists());
    assert!(root.join("r6/scripts/Lifecycle/only_in_2.reds").exists());
    let check = lib.verify_deployment(&w.game, &w.instance).unwrap();
    assert!(check.drifted.is_empty(), "{:?}", check.drifted);
    assert_eq!(lib.revisions(&mod_id).unwrap().len(), 2);

    // Purge: everything back as it was.
    let report = lib.purge(&w.game, &w.instance).unwrap();
    assert_eq!(report.restored, 1);
    assert!(lib.store().deployed_files(&id).unwrap().is_empty());
    assert_restored(&w, &files_before, &dirs_before);
    assert!(
        !Path::new(&data.join("preserved")).join(&id).exists(),
        "nothing was edited"
    );
}

#[test]
fn lifecycle_ends_byte_identical() {
    lifecycle(Trouble::None);
}

#[test]
fn lifecycle_with_a_failed_deploy_ends_byte_identical() {
    lifecycle(Trouble::FailedDeploy);
}

#[test]
fn lifecycle_with_a_crash_ends_byte_identical() {
    lifecycle(Trouble::Crash);
}
