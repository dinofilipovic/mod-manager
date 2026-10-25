//! Versions of a mod: updates replay the installer choices, rollback and
//! reinstall go back to an exact earlier state.

mod fixtures;

use std::fs;

use mm_core::env::crossover;
use mm_core::fomod::Selections;
use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta, Prepared};
use mm_core::{GameDefinition, GameInstance, scan_environments};

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

/// A FOMOD mod whose option B installs a variant archive plus a script;
/// `version` changes the bytes, `rename` renames option B.
fn fomod_mod(version: &str, rename: bool) -> fixtures::Entries {
    let b = if rename { "Option B (new)" } else { "Option B" };
    let config = format!(
        r#"<config><moduleName>Variants</moduleName><installSteps><installStep name="Pick">
        <optionalFileGroups><group name="Variant" type="SelectExactlyOne"><plugins>
          <plugin name="Option A"><description>A</description><files><file source="a/v.archive" destination="archive/pc/mod/v.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
          <plugin name="{b}"><description>B</description><files><file source="b/v.archive" destination="archive/pc/mod/v.archive"/><folder source="b/scripts" destination="r6/scripts/Variants"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
        </plugins></group></optionalFileGroups></installStep></installSteps></config>"#
    );
    vec![
        ("fomod/ModuleConfig.xml".into(), config.into_bytes()),
        ("a/v.archive".into(), format!("A {version}").into_bytes()),
        ("b/v.archive".into(), format!("B {version}").into_bytes()),
        (
            "b/scripts/main.reds".into(),
            format!("// B {version}").into_bytes(),
        ),
    ]
}

fn choose_b() -> Selections {
    Selections(vec![vec![vec![1]]])
}

fn install_v1(w: &mut World) -> String {
    let id = w.instance.id.clone();
    let v1 = fixtures::write_zip(&w.tmp.path().join("in/v1.zip"), &fomod_mod("1", false));
    let Prepared::NeedsChoices(request) = w
        .lib
        .prepare_archive(&w.game, &v1, ModMeta::default(), Some(&id))
        .unwrap()
    else {
        panic!("a first install asks");
    };
    assert!(!request.preselected);
    w.lib
        .finish_fomod(&w.game, &request.token, &choose_b())
        .unwrap()
        .record
        .id
}

#[test]
fn update_replays_choices_and_rollback_restores_the_exact_files() {
    let mut w = world();
    let root = w.instance.install_path.clone();
    let id = w.instance.id.clone();
    let mod_id = install_v1(&mut w);
    w.lib.deploy(&w.game, &w.instance).unwrap();
    let v1_state = fixtures::snapshot(&root);
    assert_eq!(
        fs::read_to_string(root.join("archive/pc/mod/v.archive")).unwrap(),
        "B 1"
    );

    // Version 2 has the same installer: the choices are applied without asking.
    let v2 = fixtures::write_7z(&w.tmp.path().join("in/v2.7z"), &fomod_mod("2", false));
    let meta = ModMeta {
        replaces: Some(mod_id.clone()),
        version: Some("2".into()),
        ..Default::default()
    };
    let Prepared::Installed(updated) = w
        .lib
        .prepare_archive(&w.game, &v2, meta, Some(&id))
        .unwrap()
    else {
        panic!("matching installer choices are replayed");
    };
    assert!(updated.replayed_choices);
    assert_eq!(
        w.lib.mod_files(&mod_id).unwrap(),
        ["archive/pc/mod/v.archive", "r6/scripts/Variants/main.reds"],
        "the same files as with option B"
    );
    w.lib.deploy(&w.game, &w.instance).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("archive/pc/mod/v.archive")).unwrap(),
        "B 2"
    );

    // Roll back: version 1's archive and choices again, as a new revision.
    let Prepared::Installed(back) = w.lib.rollback(&w.game, &mod_id, Some(&id)).unwrap() else {
        panic!("rollback replays the old choices");
    };
    assert_eq!(back.record.revision, 3);
    assert_eq!(
        back.record.version.as_deref(),
        None,
        "v1 was installed without a version"
    );
    w.lib.deploy(&w.game, &w.instance).unwrap();
    assert_eq!(
        fixtures::snapshot(&root),
        v1_state,
        "exactly the files of version 1"
    );

    // Rolling back again returns to version 2.
    w.lib.rollback(&w.game, &mod_id, Some(&id)).unwrap();
    w.lib.deploy(&w.game, &w.instance).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("archive/pc/mod/v.archive")).unwrap(),
        "B 2"
    );
}

#[test]
fn changed_installer_asks_with_matching_choices_preselected() {
    let mut w = world();
    let id = w.instance.id.clone();
    let mod_id = install_v1(&mut w);
    let v2 = fixtures::write_zip(&w.tmp.path().join("in/v2.zip"), &fomod_mod("2", true));
    let meta = ModMeta {
        replaces: Some(mod_id.clone()),
        ..Default::default()
    };
    let Prepared::NeedsChoices(request) = w
        .lib
        .prepare_archive(&w.game, &v2, meta, Some(&id))
        .unwrap()
    else {
        panic!("a renamed option means asking");
    };
    assert!(request.preselected);
    // "Option B" no longer exists, so nothing matched in that group.
    assert_eq!(request.defaults.0[0][0], Vec::<usize>::new());
    let outcome = w
        .lib
        .finish_fomod(&w.game, &request.token, &choose_b())
        .unwrap();
    assert!(outcome.replaced_existing);
}

#[test]
fn reinstall_and_change_options() {
    let mut w = world();
    let id = w.instance.id.clone();
    let mod_id = install_v1(&mut w);

    let Prepared::Installed(again) = w
        .lib
        .reinstall(&w.game, &mod_id, 1, Some(&id), false)
        .unwrap()
    else {
        panic!("a reinstall uses the same choices");
    };
    assert_eq!(again.record.revision, 2);
    assert!(again.replayed_choices);

    let Prepared::NeedsChoices(request) = w
        .lib
        .reinstall(&w.game, &mod_id, 2, Some(&id), true)
        .unwrap()
    else {
        panic!("changing options shows the installer");
    };
    assert!(request.preselected);
    assert_eq!(
        request.defaults,
        choose_b(),
        "the saved choices are preselected"
    );
    let choose_a = Selections(vec![vec![vec![0]]]);
    w.lib
        .finish_fomod(&w.game, &request.token, &choose_a)
        .unwrap();
    assert_eq!(
        w.lib.mod_files(&mod_id).unwrap(),
        ["archive/pc/mod/v.archive"]
    );
    let revisions = w.lib.revisions(&mod_id).unwrap();
    assert_eq!(revisions[0].revision, 3);
    assert_eq!(
        revisions[0].fomod_choices.as_ref().unwrap().0[0].groups[0].options,
        ["Option A"]
    );
}

#[test]
fn rollback_needs_a_kept_archive() {
    let mut w = world();
    let id = w.instance.id.clone();
    let mod_id = install_v1(&mut w);
    let err = w
        .lib
        .rollback(&w.game, &mod_id, Some(&id))
        .unwrap_err()
        .to_string();
    assert!(err.contains("No earlier version"), "{err}");
    w.lib
        .set_archive_retention(mm_core::archives::ArchiveRetention::DeleteAfterInstall)
        .unwrap();
    let err = w
        .lib
        .reinstall(&w.game, &mod_id, 1, Some(&id), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no longer kept"), "{err}");
}
