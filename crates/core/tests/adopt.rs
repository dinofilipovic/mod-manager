//! Mods installed by hand are found and adopted without touching the game folder.

mod fixtures;

use mm_core::env::crossover;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::scan_environments;

#[test]
fn hand_installed_mods_are_grouped_and_adopted_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let bottle = fixtures::fake_bottle(tmp.path(), "Cyberpunk 2077");
    let game = find_builtin("cyberpunk2077").unwrap();
    let envs = crossover::discover_bottles(&bottle.bottles_dir).unwrap();
    let instance = scan_environments(std::slice::from_ref(&game), envs)
        .instances
        .remove(0);
    let root = instance.install_path.clone();
    let mut lib = Library::open(&tmp.path().join("data")).unwrap();

    // One mod through the manager, three by hand.
    let managed = fixtures::write_zip(
        &tmp.path().join("in/managed.zip"),
        &vec![
            ("archive/pc/mod/managed.archive".into(), b"m".to_vec()),
            (
                "bin/x64/plugins/cyber_engine_tweaks/mods/Managed/init.lua".into(),
                b"return {}".to_vec(),
            ),
        ],
    );
    let id = instance.id.clone();
    lib.install_archive(&game, &managed, ModMeta::default(), Some(&id))
        .unwrap();
    lib.deploy(&game, &instance).unwrap();
    fixtures::write_files(
        &root,
        &[
            ("archive/pc/mod/HandMade.archive", b"RDAR hand".as_slice()),
            ("archive/pc/mod/HandMade.archive.xl", b"resource: {}\n"),
            // Two versions side by side are two mods.
            ("archive/pc/mod/Variant.v1.archive", b"RDAR 1"),
            ("archive/pc/mod/Variant.v2.archive", b"RDAR 2"),
            // Written by CET into the managed mod's folder: not a mod.
            (
                "bin/x64/plugins/cyber_engine_tweaks/mods/Managed/Managed.log",
                b"[info] started",
            ),
            ("r6/scripts/HandScript/main.reds", b"// hand"),
            ("r6/scripts/HandScript/lib/util.reds", b"// util"),
            (
                "bin/x64/plugins/cyber_engine_tweaks/mods/HandCet/init.lua",
                b"return {}",
            ),
        ],
    );
    let before = fixtures::snapshot(&root);

    let groups = lib.unowned_mods(&game, &instance).unwrap();
    let names: Vec<(&str, &str, usize)> = groups
        .iter()
        .map(|g| (g.kind.as_str(), g.name.as_str(), g.files.len()))
        .collect();
    assert_eq!(
        names,
        [
            ("archive", "HandMade", 2),
            ("archive", "Variant.v1", 1),
            ("archive", "Variant.v2", 1),
            ("cet", "HandCet", 1),
            ("redscript", "HandScript", 2)
        ],
        "the managed mods' files and folders aren't offered"
    );

    let script = groups.iter().find(|g| g.name == "HandScript").unwrap();
    let outcome = lib
        .adopt(&game, &instance, "Hand Script", &script.files)
        .unwrap();
    assert_eq!(
        fixtures::snapshot(&root),
        before,
        "the game folder is untouched"
    );

    let loadout = lib.loadout(&game, &instance).unwrap();
    let adopted = loadout
        .mods
        .iter()
        .find(|m| m.record.id == outcome.record.id)
        .unwrap();
    assert!(adopted.enabled);
    assert_eq!(adopted.record.kinds, ["redscript"]);
    assert!(!loadout.pending_changes, "already where it belongs");
    assert!(
        lib.verify_deployment(&game, &instance)
            .unwrap()
            .drifted
            .is_empty()
    );
    assert_eq!(lib.unowned_mods(&game, &instance).unwrap().len(), 4);
    assert!(
        lib.adopt(&game, &instance, "Again", &script.files)
            .unwrap_err()
            .to_string()
            .contains("already belongs")
    );
}
