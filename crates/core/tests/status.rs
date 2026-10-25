//! Whether mods loaded, read from logs in the frameworks' formats.

mod fixtures;

use std::fs;

use mm_core::env::crossover;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::scan_environments;
use mm_core::status::LoadStatus;

fn fixture_log(name: &str) -> Vec<u8> {
    fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/logs")
            .join(name),
    )
    .unwrap()
}

#[test]
fn logs_tell_which_mods_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let bottle = fixtures::fake_bottle(tmp.path(), "Cyberpunk 2077");
    let game = find_builtin("cyberpunk2077").unwrap();
    let envs = crossover::discover_bottles(&bottle.bottles_dir).unwrap();
    let instance = scan_environments(std::slice::from_ref(&game), envs)
        .instances
        .remove(0);
    let mut lib = Library::open(&tmp.path().join("data")).unwrap();
    let id = instance.id.clone();
    let mut install = |name: &str, files: &[&str]| {
        let entries: fixtures::Entries = files
            .iter()
            .map(|f| (f.to_string(), b"x".to_vec()))
            .collect();
        let archive = fixtures::write_zip(&tmp.path().join(format!("in/{name}.zip")), &entries);
        let meta = ModMeta {
            name: Some(name.into()),
            ..Default::default()
        };
        lib.install_archive(&game, &archive, meta, Some(&id))
            .unwrap()
            .record
            .id
    };
    let axl = install("ArchiveXL", &["red4ext/plugins/ArchiveXL/ArchiveXL.dll"]);
    let broken_plugin = install(
        "Broken Plugin",
        &["red4ext/plugins/BrokenPlugin/BrokenPlugin.dll"],
    );
    // Its self-declared name ("Mod Settings") isn't its folder's.
    let mod_settings = install(
        "Mod Settings",
        &["red4ext/plugins/mod_settings/mod_settings.dll"],
    );
    let old_plugin = install("Old Plugin", &["red4ext/plugins/OldPlugin/OldPlugin.dll"]);
    let settings = install(
        "Native Settings",
        &["bin/x64/plugins/cyber_engine_tweaks/mods/nativeSettings/init.lua"],
    );
    let broken_cet = install(
        "Broken HUD",
        &["bin/x64/plugins/cyber_engine_tweaks/mods/BrokenHud/init.lua"],
    );
    let cool = install(
        "Cool HUD",
        &["r6/scripts/CoolHud/main.reds", "r6/scripts/CoolHud/ui.reds"],
    );
    let unseen = install("Never Compiled", &["r6/scripts/NeverCompiled/main.reds"]);
    let textures = install("Textures", &["archive/pc/mod/textures.archive"]);
    lib.deploy(&game, &instance).unwrap();

    // Before the game ran: nothing seen yet.
    let statuses = lib.refresh_mod_status(&game, &instance).unwrap();
    assert_eq!(statuses[&axl].status, LoadStatus::NotSeen);
    assert!(
        statuses[&axl].detail.contains("start the game"),
        "{}",
        statuses[&axl].detail
    );
    assert_eq!(statuses[&textures].status, LoadStatus::CantVerify);

    // The game ran and wrote its logs.
    let root = &instance.install_path;
    fixtures::write_files(
        root,
        &[
            (
                "red4ext/logs/red4ext-2026-10-25-14-03-11.log",
                &fixture_log("red4ext-2026-10-25-14-03-11.log"),
            ),
            (
                "bin/x64/plugins/cyber_engine_tweaks/scripting.log",
                &fixture_log("scripting.log"),
            ),
            (
                "r6/logs/redscript_rCURRENT.log",
                &fixture_log("redscript_rCURRENT.log"),
            ),
        ],
    );
    let statuses = lib
        .refresh_mod_status_if_changed(&game, &instance)
        .unwrap()
        .expect("the logs are new");
    assert!(
        lib.refresh_mod_status_if_changed(&game, &instance)
            .unwrap()
            .is_none(),
        "nothing changed since"
    );
    let status = |id: &str| statuses[id].status;
    assert_eq!(status(&axl), LoadStatus::Loaded);
    assert_eq!(
        status(&broken_plugin),
        LoadStatus::Errors,
        "{}",
        statuses[&broken_plugin].detail
    );
    assert_eq!(status(&mod_settings), LoadStatus::Loaded);
    assert_eq!(
        status(&old_plugin),
        LoadStatus::Errors,
        "incompatible with the patch"
    );
    assert_eq!(status(&settings), LoadStatus::Loaded);
    assert_eq!(status(&broken_cet), LoadStatus::Errors);
    assert_eq!(status(&cool), LoadStatus::Loaded);
    assert_eq!(status(&unseen), LoadStatus::NotSeen);
    assert_eq!(status(&textures), LoadStatus::CantVerify);

    // A log cut short (the game crashed loading a plugin): the plugins it
    // got through still count as loaded.
    let full = String::from_utf8(fixture_log("red4ext-2026-10-25-14-03-11.log")).unwrap();
    let cut: String = full.lines().take(7).map(|l| format!("{l}\n")).collect();
    fixtures::write_files(
        root,
        &[(
            "red4ext/logs/red4ext-2026-10-25-14-03-11.log",
            cut.as_bytes(),
        )],
    );
    let statuses = lib.refresh_mod_status(&game, &instance).unwrap();
    assert_eq!(statuses[&axl].status, LoadStatus::Loaded);
    assert_eq!(statuses[&mod_settings].status, LoadStatus::Loaded);
    assert_eq!(statuses[&broken_plugin].status, LoadStatus::Errors);
    assert_eq!(statuses[&old_plugin].status, LoadStatus::NotSeen);

    // The mod list shows them.
    let loadout = lib.loadout(&game, &instance).unwrap();
    let shown = loadout.mods.iter().find(|m| m.record.id == cool).unwrap();
    assert_eq!(shown.status.as_ref().unwrap().status, LoadStatus::Loaded);

    // Applying again makes these logs old news.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let id = instance.id.clone();
    lib.set_mod_enabled(&game, &id, &textures, false).unwrap();
    lib.deploy(&game, &instance).unwrap();
    let statuses = lib.refresh_mod_status(&game, &instance).unwrap();
    assert_eq!(statuses[&cool].status, LoadStatus::NotSeen);
}
