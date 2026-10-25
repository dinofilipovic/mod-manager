//! Shared helpers for integration tests: a fake CrossOver bottle with a
//! Cyberpunk 2077 install, and mod archives built in code in the shapes found
//! on Nexus (game-root layout, wrapper folder, loose files, FOMOD, mixed case).

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Files of an archive: path inside the archive -> contents.
pub type Entries = Vec<(String, Vec<u8>)>;

/// A fake CrossOver bottle folder holding one bottle with the game installed
/// at a known GOG path, so detection finds it without store metadata.
pub struct FakeBottle {
    /// Pass this as `--bottles-dir` / to `discover_bottles`.
    pub bottles_dir: PathBuf,
    pub bottle: PathBuf,
    pub game_dir: PathBuf,
}

pub const BOTTLE_CONF: &str = r#"[Bottle]
"BottleID" = "00000000"
"Version" = "26.0.0.0"
"Template" = "win10_64"
"WineArch" = "win64"
"#;

/// Builds a bottle named `name` under `root/Bottles` with the game's
/// executable and a few original game files.
pub fn fake_bottle(root: &Path, name: &str) -> FakeBottle {
    let bottles_dir = root.join("Bottles");
    let bottle = bottles_dir.join(name);
    fs::create_dir_all(bottle.join("drive_c/users/crossover")).unwrap();
    fs::write(bottle.join("cxbottle.conf"), BOTTLE_CONF).unwrap();
    fs::write(bottle.join("system.reg"), "WINE REGISTRY Version 2\n").unwrap();
    fs::write(
        bottle.join("user.reg"),
        "WINE REGISTRY Version 2\n\n[Software\\\\Wine\\\\DllOverrides] 1\n\"version\"=\"native,builtin\"\n\"winmm\"=\"native,builtin\"\n",
    )
    .unwrap();
    let game_dir = bottle.join("drive_c/GOG Games/Cyberpunk 2077");
    write_files(
        &game_dir,
        &[
            (
                "bin/x64/Cyberpunk2077.exe",
                b"MZ fake game executable".as_slice(),
            ),
            (
                "archive/pc/content/basegame_1_engine.archive",
                b"RDAR original content",
            ),
            ("engine/config/base/general.ini", b"[General]\nOriginal=1\n"),
            (
                "r6/config/settings/platform/pc/options.json",
                b"{\"original\":true}",
            ),
        ],
    );
    FakeBottle {
        bottles_dir,
        bottle,
        game_dir,
    }
}

pub fn write_files(root: &Path, files: &[(&str, &[u8])]) {
    for (relative, data) in files {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
}

fn entries(files: &[(&str, &str)]) -> Entries {
    files
        .iter()
        .map(|(p, d)| (p.to_string(), d.as_bytes().to_vec()))
        .collect()
}

/// Laid out from the game root: `archive/pc/mod/...`, `r6/scripts/...`.
pub fn game_root_layout(tag: &str) -> Entries {
    entries(&[
        (
            &format!("archive/pc/mod/{tag}.archive"),
            &format!("RDAR {tag} archive"),
        ),
        (&format!("r6/scripts/{tag}/main.reds"), "module Test\n"),
        ("readme.txt", "not a game file"),
    ])
}

/// Everything inside an extra top folder, as many authors pack it.
pub fn wrapper_folder(tag: &str) -> Entries {
    game_root_layout(tag)
        .into_iter()
        .map(|(p, d)| (format!("{tag} v1.0/{p}"), d))
        .collect()
}

/// Loose files the installer routes by extension and marker file.
pub fn loose_files(tag: &str) -> Entries {
    entries(&[
        (&format!("{tag}.archive"), &format!("RDAR loose {tag}")),
        (&format!("{tag}/init.lua"), "return {}\n"),
        (&format!("{tag}/settings.json"), "{\"enabled\":true}"),
    ])
}

/// Folders in other casing than the game uses (`Archive/PC/Mod`).
pub fn mixed_case(tag: &str) -> Entries {
    entries(&[(
        &format!("Archive/PC/Mod/{tag}.archive"),
        &format!("RDAR mixed {tag}"),
    )])
}

/// A FOMOD installer with one step choosing between two archives.
pub fn fomod(tag: &str) -> Entries {
    let config = format!(
        r#"<config><moduleName>{tag}</moduleName><installSteps><installStep name="Pick">
        <optionalFileGroups><group name="Variant" type="SelectExactlyOne"><plugins>
          <plugin name="Option A"><description>A</description><files><file source="a/{tag}.archive" destination="archive/pc/mod/{tag}.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
          <plugin name="Option B"><description>B</description><files><file source="b/{tag}.archive" destination="archive/pc/mod/{tag}.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
        </plugins></group></optionalFileGroups></installStep></installSteps></config>"#
    );
    vec![
        ("fomod/ModuleConfig.xml".into(), config.into_bytes()),
        (
            format!("a/{tag}.archive"),
            format!("RDAR {tag} A").into_bytes(),
        ),
        (
            format!("b/{tag}.archive"),
            format!("RDAR {tag} B").into_bytes(),
        ),
    ]
}

pub fn write_zip(path: &Path, files: &Entries) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, data) in files {
        zip.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    path.to_path_buf()
}

pub fn write_7z(path: &Path, files: &Entries) -> PathBuf {
    let src = tempfile::tempdir().unwrap();
    for (name, data) in files {
        let p = src.path().join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    sevenz_rust2::compress_to_path(src.path(), path).unwrap();
    path.to_path_buf()
}

/// Every file under `root` with its bytes, keyed by `/`-separated relative
/// path. Two snapshots are equal when the folders are byte-identical.
pub fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let relative = e
                .path()
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            (relative, fs::read(e.path()).unwrap())
        })
        .collect()
}

/// Folders under `root`, so a purge that leaves empty folders behind fails too.
pub fn dir_snapshot(root: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_dir())
        .map(|e| {
            e.path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    dirs.sort();
    dirs
}
