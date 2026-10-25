//! Every game definition in the workspace `games/` folder parses, passes
//! validation, and has a unique id matching its file name.

use std::collections::BTreeSet;
use std::path::PathBuf;

use mm_core::game::GameDefinition;

fn games_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../games")
}

#[test]
fn all_game_definitions_are_valid() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(games_dir())
        .expect("games/ folder should exist")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no game definitions found");

    let mut ids = BTreeSet::new();
    for path in &paths {
        let text = std::fs::read_to_string(path).unwrap();
        let game = GameDefinition::from_toml(&text)
            .unwrap_or_else(|e| panic!("{} is invalid: {e}", path.display()));
        let stem = path.file_stem().unwrap().to_string_lossy();
        assert_eq!(
            game.id,
            stem,
            "{}: id must match the file name",
            path.display()
        );
        assert!(
            ids.insert(game.id.clone()),
            "duplicate game id '{}'",
            game.id
        );
    }
}
