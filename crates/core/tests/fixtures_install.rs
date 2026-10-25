//! The test fixtures themselves: the fake bottle is detected like a real one
//! and every archive shape installs to the expected game paths.

mod fixtures;

use mm_core::env::crossover;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::{Store, scan_environments};

#[test]
fn fake_bottle_is_detected_through_known_install_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let bottle = fixtures::fake_bottle(tmp.path(), "Cyberpunk 2077");
    let envs = crossover::discover_bottles(&bottle.bottles_dir).unwrap();
    assert_eq!(envs.len(), 1);
    let game = find_builtin("cyberpunk2077").unwrap();
    let scan = scan_environments(std::slice::from_ref(&game), envs);
    assert_eq!(scan.instances.len(), 1);
    let instance = &scan.instances[0];
    assert_eq!(instance.store, Store::KnownPath);
    assert_eq!(
        instance.install_path,
        std::fs::canonicalize(&bottle.game_dir).unwrap()
    );
    assert!(instance.dll_overrides.iter().all(|d| d.satisfied));
}

#[test]
fn every_archive_shape_installs() {
    let tmp = tempfile::tempdir().unwrap();
    let bottle = fixtures::fake_bottle(tmp.path(), "Cyberpunk 2077");
    let game = find_builtin("cyberpunk2077").unwrap();
    let envs = crossover::discover_bottles(&bottle.bottles_dir).unwrap();
    let instance = scan_environments(std::slice::from_ref(&game), envs)
        .instances
        .remove(0);
    let mut lib = Library::open(&tmp.path().join("data")).unwrap();
    let archives = tmp.path().join("archives");

    let cases: Vec<(&str, fixtures::Entries, &[&str])> = vec![
        (
            "Root.zip",
            fixtures::game_root_layout("root"),
            &["archive/pc/mod/root.archive", "r6/scripts/root/main.reds"],
        ),
        (
            "Wrapped.7z",
            fixtures::wrapper_folder("wrap"),
            &["archive/pc/mod/wrap.archive", "r6/scripts/wrap/main.reds"],
        ),
        (
            "Loose.zip",
            fixtures::loose_files("loose"),
            &[
                "archive/pc/mod/loose.archive",
                "bin/x64/plugins/cyber_engine_tweaks/mods/loose/init.lua",
                "bin/x64/plugins/cyber_engine_tweaks/mods/loose/settings.json",
            ],
        ),
        (
            "Mixed.zip",
            fixtures::mixed_case("mixed"),
            &["archive/pc/mod/mixed.archive"],
        ),
        (
            "Fomod.7z",
            fixtures::fomod("fomod"),
            &["archive/pc/mod/fomod.archive"],
        ),
    ];
    for (name, entries, expected) in cases {
        let path = archives.join(name);
        if name.ends_with(".7z") {
            fixtures::write_7z(&path, &entries);
        } else {
            fixtures::write_zip(&path, &entries);
        }
        let outcome = lib
            .install_archive(&game, &path, ModMeta::default(), Some(&instance.id))
            .unwrap();
        let mut files = lib.mod_files(&outcome.record.id).unwrap();
        files.sort();
        let mut expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected.sort();
        assert_eq!(files, expected, "{name}");
    }
}
