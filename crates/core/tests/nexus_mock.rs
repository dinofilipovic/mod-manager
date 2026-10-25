//! Nexus Mods features against a local mock server: update checks with as
//! few requests as possible, rate limits, MD5 lookup and changelogs.

mod fixtures;

use mm_core::game::find_builtin;
use mm_core::library::{Library, ModMeta};
use mm_core::nexus::{self, NexusClient};
use mm_core::updates;
use mockito::Matcher;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nexus")
            .join(name),
    )
    .unwrap()
}

fn client(server: &mockito::Server) -> NexusClient {
    NexusClient::with_base_url("test-key", "0.0.0", &server.url()).unwrap()
}

/// A library with `count` Nexus mods (`mod_id` 1000 + i, file 1 + i).
fn library_with_mods(tmp: &std::path::Path, count: u32) -> Library {
    let game = find_builtin("cyberpunk2077").unwrap();
    let mut lib = Library::open(&tmp.join("data")).unwrap();
    for i in 0..count {
        let archive = fixtures::write_zip(
            &tmp.join(format!("in/m{i}.zip")),
            &vec![(
                format!("archive/pc/mod/m{i}.archive"),
                format!("m{i}").into_bytes(),
            )],
        );
        let meta = ModMeta {
            name: Some(format!("Mod {i}")),
            version: Some("1.0".into()),
            nexus_mod_id: Some(1000 + i),
            nexus_file_id: Some(1 + i),
            ..Default::default()
        };
        lib.install_archive(&game, &archive, meta, None).unwrap();
    }
    lib
}

fn files_json(file_id: u32, newer_id: u32, uploaded: i64) -> String {
    format!(
        r#"{{"files":[
            {{"file_id":{file_id},"name":"Main","version":"1.0","category_name":"MAIN","file_name":"a.zip","size_kb":1,"uploaded_timestamp":{}}},
            {{"file_id":{newer_id},"name":"Main","version":"1.1","category_name":"MAIN","file_name":"b.zip","size_kb":1,"uploaded_timestamp":{uploaded}}}],
          "file_updates":[{{"old_file_id":{file_id},"new_file_id":{newer_id},"old_file_name":"a.zip","new_file_name":"b.zip","uploaded_timestamp":{uploaded},"uploaded_time":"2026-10-20T00:00:00.000+00:00"}}]}}"#,
        uploaded - 1000
    )
}

#[test]
fn update_check_of_50_mods_with_3_changed_makes_4_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let game = find_builtin("cyberpunk2077").unwrap();
    let lib = library_with_mods(tmp.path(), 50);
    let now = 1_761_400_000;

    // A previous check two days ago saw every mod's files up to `seen`.
    let seen = now - 3 * 86_400;
    let mut check = lib.update_check(&game).unwrap().unwrap();
    let outcome = updates::CheckOutcome {
        game_id: game.id.clone(),
        checked_at: now - 2 * 86_400,
        mods: check
            .mods
            .iter()
            .map(|m| updates::ModUpdate {
                mod_id: m.id.clone(),
                nexus_updated_at: Some(seen),
                update: None,
            })
            .collect(),
        requests: 0,
    };
    lib.apply_update_check(&outcome, None).unwrap();
    check = lib.update_check(&game).unwrap().unwrap();
    assert_eq!(
        updates::period_since(check.last_checked_at, now),
        Some("1w")
    );

    let mut server = mockito::Server::new();
    // Ten mods had activity; three of them got a file newer than `seen`.
    let changed = [1003u32, 1020, 1047];
    let entries: Vec<String> = (1000..1010)
        .chain(changed)
        .map(|id| {
            let t = if changed.contains(&id) {
                now - 3600
            } else {
                seen - 10
            };
            format!(r#"{{"mod_id":{id},"latest_file_update":{t},"latest_mod_activity":{t}}}"#)
        })
        .collect();
    let updated = server
        .mock("GET", "/games/cyberpunk2077/mods/updated.json")
        .match_query(Matcher::UrlEncoded("period".into(), "1w".into()))
        .match_header("apikey", "test-key")
        .with_body(format!("[{}]", entries.join(",")))
        .expect(1)
        .create();
    let files: Vec<mockito::Mock> = changed
        .iter()
        .map(|&id| {
            let file = id - 1000 + 1;
            server
                .mock(
                    "GET",
                    format!("/games/cyberpunk2077/mods/{id}/files.json").as_str(),
                )
                .with_body(files_json(file, file + 500, now - 3600))
                .expect(1)
                .create()
        })
        .collect();
    let any_other = server
        .mock("GET", Matcher::Regex(r"/files\.json$".into()))
        .expect(0)
        .create();

    let outcome = updates::run(&client(&server), &check, now).unwrap();
    assert_eq!(outcome.requests, 4);
    updated.assert();
    for m in &files {
        m.assert();
    }
    any_other.assert();

    let summary = lib.apply_update_check(&outcome, Some("1w")).unwrap();
    assert_eq!(
        (summary.checked, summary.available, summary.requests),
        (3, 3, 4)
    );
    let with_update: Vec<u32> = lib
        .mods_for_game(&game.id)
        .unwrap()
        .iter()
        .filter(|m| m.update_file_id.is_some())
        .filter_map(|m| m.nexus_mod_id)
        .collect();
    assert_eq!(with_update.len(), 3);
    assert!(changed.iter().all(|c| with_update.contains(c)));
    assert_eq!(
        lib.update_counts().unwrap()["cyberpunk2077"],
        3,
        "the sidebar badge"
    );

    // Automatic checks run at most once a day, and can be turned off.
    assert!(!lib.update_check_due(&game.id, now + 3600));
    assert!(lib.update_check_due(&game.id, now + 86_400));
    lib.set_auto_update_check(false).unwrap();
    assert!(!lib.update_check_due(&game.id, now + 86_400));
}

#[test]
fn first_check_looks_at_every_mod_and_respects_pins_and_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let game = find_builtin("cyberpunk2077").unwrap();
    let lib = library_with_mods(tmp.path(), 3);
    let mods = lib.mods_for_game(&game.id).unwrap();
    lib.set_mod_pinned(&mods[1].id, true).unwrap();

    let mut server = mockito::Server::new();
    let no_period = server
        .mock("GET", "/games/cyberpunk2077/mods/updated.json")
        .match_query(Matcher::Any)
        .expect(0)
        .create();
    for (i, m) in mods.iter().enumerate() {
        let file = i as u32 + 1;
        server
            .mock(
                "GET",
                format!(
                    "/games/cyberpunk2077/mods/{}/files.json",
                    m.nexus_mod_id.unwrap()
                )
                .as_str(),
            )
            .with_body(files_json(file, file + 500, 1_700_000_000))
            .expect(2)
            .create();
    }
    let check = lib.update_check(&game).unwrap().unwrap();
    assert_eq!(check.last_checked_at, None);
    let outcome = updates::run(&client(&server), &check, 1_761_400_000).unwrap();
    assert_eq!(outcome.requests, 3, "a full check, one file list per mod");
    no_period.assert();
    lib.apply_update_check(&outcome, None).unwrap();

    let after = lib.mods_for_game(&game.id).unwrap();
    assert_eq!(after[0].update_file_id, Some(501));
    assert_eq!(after[1].update_file_id, None, "pinned");
    assert_eq!(after[0].nexus_updated_at, Some(1_700_000_000));

    // Skipping the update file hides it, also on the next full check.
    lib.ignore_update(&after[0].id, true).unwrap();
    let check = lib.update_check(&game).unwrap().unwrap();
    let mut check = check;
    check.last_checked_at = None;
    let outcome = updates::run(&client(&server), &check, 1_761_400_000).unwrap();
    lib.apply_update_check(&outcome, None).unwrap();
    let after = lib.mods_for_game(&game.id).unwrap();
    assert_eq!(after[0].update_file_id, None);
    assert_eq!(after[2].update_file_id, Some(503));
}

#[test]
fn reads_rate_limits_and_pauses_background_work() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/users/validate.json")
        .with_header("X-RL-Hourly-Limit", "100")
        .with_header("X-RL-Hourly-Remaining", "12")
        .with_header("X-RL-Hourly-Reset", "2099-02-01T12:00:00+00:00")
        .with_header("X-RL-Daily-Limit", "2500")
        .with_header("X-RL-Daily-Remaining", "0")
        .with_header("X-RL-Daily-Reset", "2099-02-02 00:00:00 +0000")
        .with_body(r#"{"user_id":1,"name":"demo","is_premium":false,"profile_url":null}"#)
        .create();
    client(&server).validate().unwrap();
    let limits = nexus::rate_limits().unwrap();
    assert_eq!((limits.hourly_remaining, limits.daily_remaining), (12, 0));
    assert_eq!(
        limits.hourly_reset,
        nexus::parse_reset_time("2099-02-01 12:00:00 +0000")
    );
    assert_eq!(limits.available(), 12);
    let err = nexus::check_budget(limits.observed_at)
        .unwrap_err()
        .to_string();
    assert!(err.contains("only 12 are left"), "{err}");
}

#[test]
fn finds_archives_by_md5_and_reads_changelogs() {
    let mut server = mockito::Server::new();
    let _md5 = server
        .mock(
            "GET",
            "/games/cyberpunk2077/mods/md5_search/0f343b0931126a20f133d67c2b018a3b.json",
        )
        .with_body(fixture("md5_search.json"))
        .create();
    let _unknown = server
        .mock(
            "GET",
            "/games/cyberpunk2077/mods/md5_search/ffffffffffffffffffffffffffffffff.json",
        )
        .with_status(404)
        .with_body(r#"{"message":"No file found with md5"}"#)
        .create();
    let _log = server
        .mock("GET", "/games/cyberpunk2077/mods/4198/changelogs.json")
        .with_body(fixture("changelogs.json"))
        .create();
    let c = client(&server);
    let found = c
        .identify_by_md5("cyberpunk2077", "0f343b0931126a20f133d67c2b018a3b")
        .unwrap()
        .unwrap();
    assert_eq!((found.mod_id, found.file_id), (4198, Some(130001)));
    assert_eq!(found.name, "ArchiveXL");
    assert!(
        c.identify_by_md5("cyberpunk2077", "ffffffffffffffffffffffffffffffff")
            .unwrap()
            .is_none()
    );
    let log = c.changelogs("cyberpunk2077", 4198).unwrap();
    assert_eq!(log["1.28.0"][0], "Updated compatibility to game patch 2.5");
}

/// Against the real API, with a key from `NEXUS_API_KEY`.
#[test]
#[ignore = "needs the network and a Nexus API key"]
fn live_updated_mods() {
    let key = std::env::var("NEXUS_API_KEY").expect("set NEXUS_API_KEY");
    let c = NexusClient::new(&key, "0.0.0").unwrap();
    let entries = c.updated_mods("cyberpunk2077", "1d").unwrap();
    assert!(!entries.is_empty());
    assert!(nexus::rate_limits().is_some());
}

#[test]
fn download_resumes_a_part_file_with_a_range_request() {
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
    let dest = tmp.path().join("mod.zip");
    let half = body.len() / 2;
    std::fs::write(nexus::part_path(&dest), &body[..half]).unwrap();

    let mut server = mockito::Server::new();
    let resumed = server
        .mock("GET", "/file/mod.zip")
        .match_header("range", format!("bytes={half}-").as_str())
        .with_status(206)
        .with_header(
            "content-range",
            &format!("bytes {half}-{}/{}", body.len() - 1, body.len()),
        )
        .with_body(&body[half..])
        .expect(1)
        .create();
    let mut seen = Vec::new();
    let hashes = client(&server)
        .download(
            &format!("{}/file/mod.zip", server.url()),
            &dest,
            |done, total| {
                seen.push((done, total));
                true
            },
        )
        .unwrap();
    resumed.assert();
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert!(!nexus::part_path(&dest).exists());
    assert_eq!(hashes.size, body.len() as u64);
    let tmp_copy = tmp.path().join("copy");
    std::fs::write(&tmp_copy, &body).unwrap();
    assert_eq!(
        hashes.sha256,
        mm_core::fs_util::sha256_file(&tmp_copy).unwrap(),
        "the hash covers the bytes from before the restart too"
    );
    assert_eq!(seen.first().unwrap().1, Some(body.len() as u64));
}

#[test]
fn download_starts_over_when_the_server_ignores_the_range() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("mod.zip");
    std::fs::write(nexus::part_path(&dest), b"stale bytes").unwrap();
    let mut server = mockito::Server::new();
    let _full = server
        .mock("GET", "/file/mod.zip")
        .with_status(200)
        .with_body("fresh whole file")
        .create();
    client(&server)
        .download(&format!("{}/file/mod.zip", server.url()), &dest, |_, _| {
            true
        })
        .unwrap();
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "fresh whole file");

    // Server trouble is worth retrying; an expired link isn't.
    let _gone = server
        .mock("GET", "/file/gone.zip")
        .with_status(410)
        .create();
    let _busy = server
        .mock("GET", "/file/busy.zip")
        .with_status(503)
        .create();
    let gone = client(&server)
        .download(
            &format!("{}/file/gone.zip", server.url()),
            &tmp.path().join("g"),
            |_, _| true,
        )
        .unwrap_err();
    let busy = client(&server)
        .download(
            &format!("{}/file/busy.zip", server.url()),
            &tmp.path().join("b"),
            |_, _| true,
        )
        .unwrap_err();
    assert!(!mm_core::queue::is_retryable(&gone));
    assert!(mm_core::queue::is_retryable(&busy));
}

#[test]
fn download_queue_survives_a_restart() {
    use mm_core::nexus::{Download, DownloadStatus};
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("data");
    let now = 1_761_400_000;
    {
        let lib = Library::open(&root).unwrap();
        let base = Download {
            game_id: "cyberpunk2077".into(),
            game_domain: "cyberpunk2077".into(),
            name: "A mod".into(),
            nexus_mod_id: 1,
            nexus_file_id: 2,
            ..Default::default()
        };
        for (id, status, key_expires, created_at) in [
            ("downloading", DownloadStatus::Downloading, None, 1),
            ("waiting", DownloadStatus::Queued, None, 2),
            ("old-link", DownloadStatus::Queued, Some(now as u64 - 10), 3),
            (
                "done",
                DownloadStatus::Done {
                    mod_id: "m".into(),
                    replaced_existing: false,
                    replayed_choices: false,
                    warnings: vec![],
                },
                None,
                4,
            ),
        ] {
            lib.store()
                .save_download(&Download {
                    id: id.into(),
                    status,
                    key: key_expires.map(|_| "key".into()),
                    expires: key_expires,
                    bytes_done: 1000,
                    created_at,
                    ..base.clone()
                })
                .unwrap();
        }
    }
    // The app was closed; it opens the library again.
    let lib = Library::open(&root).unwrap();
    let queue = mm_core::queue::restore(lib.store().downloads().unwrap(), now);
    let states: Vec<(&str, &str)> = queue
        .iter()
        .map(|d| {
            let state = match &d.status {
                DownloadStatus::Queued => "queued",
                DownloadStatus::Failed { .. } => "failed",
                DownloadStatus::Done { .. } => "done",
                _ => "other",
            };
            (d.id.as_str(), state)
        })
        .collect();
    assert_eq!(
        states,
        [
            ("downloading", "queued"),
            ("waiting", "queued"),
            ("old-link", "failed"),
            ("done", "done")
        ]
    );
    assert_eq!(queue[0].bytes_done, 1000, "progress kept for the resume");
    assert_eq!(mm_core::queue::next_ready(&queue, now), Some(0));
}
