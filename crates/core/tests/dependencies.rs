//! Requirements end to end: Nexus requirements from the (mocked) v2 API,
//! the user's own, and what the active profile makes of them.

mod fixtures;

use mm_core::dependencies::{self, DependencyKind, DependencyState};
use mm_core::env::crossover;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::{GameDefinition, GameInstance, scan_environments};
use mockito::Matcher;

struct World {
    tmp: tempfile::TempDir,
    lib: Library,
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
        lib: Library::open(&tmp.path().join("data")).unwrap(),
        tmp,
        game,
        instance,
    }
}

fn install(w: &mut World, name: &str, nexus: u32, files: &[&str]) -> String {
    let entries: fixtures::Entries = files
        .iter()
        .map(|f| (f.to_string(), name.as_bytes().to_vec()))
        .collect();
    let archive = fixtures::write_zip(&w.tmp.path().join(format!("in/{name}.zip")), &entries);
    let meta = ModMeta {
        name: Some(name.into()),
        version: Some("1.0".into()),
        nexus_mod_id: Some(nexus),
        nexus_file_id: Some(nexus * 10),
        ..Default::default()
    };
    let id = w.instance.id.clone();
    w.lib
        .install_archive(&w.game, &archive, meta, Some(&id))
        .unwrap()
        .record
        .id
}

fn recorded_response() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nexus/requirements_response.json"),
    )
    .unwrap()
}

#[test]
fn nexus_requirements_feed_the_profile() {
    let mut w = world();
    let settings = install(
        &mut w,
        "Mod Settings",
        4885,
        &["red4ext/plugins/mod_settings/mod_settings.dll"],
    );
    let axl = install(
        &mut w,
        "ArchiveXL",
        4198,
        &["red4ext/plugins/ArchiveXL/ArchiveXL.dll"],
    );
    let r4 = install(&mut w, "RED4ext", 2380, &["red4ext/RED4ext.dll"]);

    let mut server = mockito::Server::new();
    let requested = server
        .mock("POST", "/graphql")
        .match_body(Matcher::PartialJsonString(
            r#"{"variables":{"ids":[{"gameDomain":"cyberpunk2077","modId":4885}]}}"#.into(),
        ))
        .with_body(recorded_response())
        .expect(1)
        .create();
    let found = dependencies::fetch_requirements(
        &format!("{}/graphql", server.url()),
        None,
        "cyberpunk2077",
        &[4885],
        "0.0.0",
    )
    .unwrap();
    requested.assert();
    let targets = w.lib.requirement_targets(&w.game.id, false).unwrap();
    assert_eq!(targets.len(), 3, "nothing fetched yet");
    // The response has Mod Settings and ArchiveXL, not RED4ext.
    assert_eq!(
        w.lib.apply_nexus_requirements(&w.game.id, &found).unwrap(),
        2
    );
    let left = w.lib.requirement_targets(&w.game.id, false).unwrap();
    assert_eq!(
        left,
        [(r4.clone(), 2380)],
        "only RED4ext still to ask about"
    );

    let loadout = w.lib.loadout(&w.game, &w.instance).unwrap();
    let entry = |id: &str| loadout.mods.iter().find(|m| m.record.id == id).unwrap();
    let states: Vec<(String, &DependencyState)> = entry(&settings)
        .requires
        .iter()
        .filter(|d| d.kind == DependencyKind::NexusMod)
        .map(|d| (d.name.clone(), &d.state))
        .collect();
    assert_eq!(states.len(), 3);
    let state = |name: &str| states.iter().find(|(n, _)| n == name).unwrap().1;
    // RED4ext and ArchiveXL are installed; redscript isn't.
    assert!(matches!(
        state("RED4ext"),
        DependencyState::Satisfied { .. }
    ));
    assert!(matches!(
        state("ArchiveXL"),
        DependencyState::Satisfied { .. }
    ));
    assert_eq!(state("redscript"), &DependencyState::Missing);
    assert!(entry(&r4).required_by.contains(&"Mod Settings".to_string()));
    assert!(entry(&r4).required_by.contains(&"ArchiveXL".to_string()));

    // Disabling or removing RED4ext would leave these without it.
    let dependents = w.lib.dependents(&w.game, &w.instance, &r4).unwrap();
    assert_eq!(dependents.len(), 2);
    let id = w.instance.id.clone();
    w.lib.set_mod_enabled(&w.game, &id, &r4, false).unwrap();
    let loadout = w.lib.loadout(&w.game, &w.instance).unwrap();
    let on_axl = loadout.mods.iter().find(|m| m.record.id == axl).unwrap();
    assert!(
        on_axl
            .requires
            .iter()
            .any(|d| matches!(&d.state, DependencyState::Disabled { mod_id, .. } if *mod_id == r4)),
        "the fix is enabling RED4ext"
    );
}

#[test]
fn user_requirements_and_cycles() {
    let mut w = world();
    let a = install(&mut w, "A", 1, &["archive/pc/mod/a.archive"]);
    let b = install(&mut w, "B", 2, &["archive/pc/mod/b.archive"]);
    w.lib
        .add_user_dependency(&a, DependencyKind::Mod, &b, false)
        .unwrap();
    w.lib
        .add_user_dependency(&b, DependencyKind::NexusMod, "1", false)
        .unwrap();
    assert!(
        w.lib
            .add_user_dependency(&a, DependencyKind::Mod, &a, false)
            .is_err()
    );
    assert!(
        w.lib
            .add_user_dependency(&a, DependencyKind::NexusMod, "not a number", false)
            .unwrap_err()
            .to_string()
            .contains("Nexus mod id")
    );
    let loadout = w.lib.loadout(&w.game, &w.instance).unwrap();
    assert_eq!(
        loadout.dependency_cycles,
        [vec!["A".to_string(), "B".to_string()]]
    );
    w.lib
        .remove_user_dependency(&b, DependencyKind::NexusMod, "1")
        .unwrap();
    let loadout = w.lib.loadout(&w.game, &w.instance).unwrap();
    assert!(loadout.dependency_cycles.is_empty());
    assert_eq!(w.lib.dependents(&w.game, &w.instance, &b).unwrap(), ["A"]);
}
