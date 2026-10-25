//! Nexus Mods Collections: collection links and the v2 GraphQL API.
//!
//! A collection is a curated list of mod files, published in numbered
//! revisions. This module only reads the list of files in a revision; each
//! file is then downloaded through the v1 API like any other mod file.
//!
//! Reading a public collection needs no API key. Collections marked as adult
//! content are only returned because the query asks for them
//! (`viewAdultContent: true`).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error::{Error, Result};

const GRAPHQL: &str = "https://api.nexusmods.com/v2/graphql";
const APP_NAME: &str = "ModManager";

/// Asks for one revision of a collection, or the latest published one when
/// `$revision` is null.
const QUERY: &str = "\
query CollectionRevision($slug: String!, $revision: Int, $domainName: String) {
  collectionRevision(slug: $slug, revision: $revision, domainName: $domainName, viewAdultContent: true) {
    revisionNumber
    collection {
      slug
      name
      summary
      user { name }
      game { domainName }
    }
    modFiles {
      fileId
      optional
      version
      file {
        fileId
        modId
        name
        version
        uri
        sizeInBytes
        mod { name game { domainName } }
      }
    }
  }
}";

/// A collection link: `nxm://<game>/collections/<slug>/revisions/<n>` or a
/// collection page such as
/// `https://www.nexusmods.com/games/<game>/collections/<slug>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub struct CollectionLink {
    pub game_domain: String,
    pub slug: String,
    /// `None` means the latest published revision.
    pub revision: Option<u32>,
}

/// One revision of a collection and the mod files it contains.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Collection {
    pub slug: String,
    pub name: String,
    pub author: String,
    pub summary: Option<String>,
    pub revision: u32,
    pub game_domain: String,
    pub mods: Vec<CollectionMod>,
    /// Files listed in the collection that Nexus no longer serves (deleted
    /// or hidden by their author). They can't be downloaded.
    pub unavailable_file_ids: Vec<u32>,
}

/// One mod file in a collection.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct CollectionMod {
    pub mod_id: u32,
    pub file_id: u32,
    /// The mod's name.
    pub name: String,
    /// The file's title on the mod's Files tab.
    pub file_title: String,
    /// The archive's file name, e.g. `redscript-1511-0-5-31-1757113888.zip`.
    pub file_name: Option<String>,
    pub version: Option<String>,
    #[ts(type = "number | null")]
    pub size_bytes: Option<u64>,
    pub optional: bool,
    /// The game the mod belongs to (normally the collection's game).
    pub game_domain: String,
}

/// Parses a collection link: an `nxm://` collection link or an https
/// collection page URL (`www.nexusmods.com/games/<game>/collections/<slug>`,
/// the older `www.nexusmods.com/<game>/collections/<slug>` and
/// `next.nexusmods.com/<game>/collections/<slug>`), optionally followed by
/// `/revisions/<n>`.
pub fn parse_collection_link(link: &str) -> Result<CollectionLink> {
    let bad = || Error::Nexus(format!("not a Nexus Mods collection link: {link}"));
    let url = url::Url::parse(link.trim()).map_err(|_| bad())?;
    let segments: Vec<&str> = url
        .path_segments()
        .map(|s| s.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    let (game_domain, rest): (&str, &[&str]) = match url.scheme() {
        "nxm" => (url.host_str().ok_or_else(bad)?, &segments),
        "http" | "https" => {
            let host = url.host_str().ok_or_else(bad)?.to_ascii_lowercase();
            if host != "nexusmods.com" && !host.ends_with(".nexusmods.com") {
                return Err(bad());
            }
            match segments.as_slice() {
                ["games", game, rest @ ..] => (game, rest),
                [game, rest @ ..] => (game, rest),
                [] => return Err(bad()),
            }
        }
        _ => return Err(bad()),
    };

    let (slug, revision) = match rest {
        ["collections", slug, "revisions", revision, ..] => (
            *slug,
            match *revision {
                "latest" => None,
                n => Some(n.parse().map_err(|_| bad())?),
            },
        ),
        ["collections", slug, ..] => (*slug, None),
        _ => return Err(bad()),
    };
    let valid = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if !valid(game_domain) || !valid(slug) {
        return Err(bad());
    }
    Ok(CollectionLink {
        game_domain: game_domain.to_ascii_lowercase(),
        slug: slug.to_string(),
        revision,
    })
}

/// Fetches the list of mod files in a collection revision. The API key is
/// optional: public collections can be read without one.
pub fn fetch_collection(
    api_key: Option<&str>,
    link: &CollectionLink,
    app_version: &str,
) -> Result<Collection> {
    let http = reqwest::blocking::Client::builder()
        .user_agent(format!("{APP_NAME}/{app_version}"))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| Error::Nexus(e.to_string()))?;
    let body = json!({
        "query": QUERY,
        "variables": {
            "slug": link.slug,
            "revision": link.revision,
            "domainName": link.game_domain,
        },
    });
    let mut request = http
        .post(GRAPHQL)
        .header("Application-Name", APP_NAME)
        .header("Application-Version", app_version)
        .json(&body);
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        request = request.header("apikey", key);
    }
    let response = request.send().map_err(|e| {
        Error::Nexus(if e.is_timeout() {
            "Nexus Mods didn't answer in time. Check your internet connection and try again.".into()
        } else {
            format!("Couldn't reach Nexus Mods. Check your internet connection. ({e})")
        })
    })?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|e| Error::Nexus(format!("unexpected response from Nexus Mods: {e}")))?;
    if !status.is_success() {
        // GraphQL errors normally come with status 200, but a body with
        // `errors` on another status still says best what went wrong.
        if let Some(message) = graphql_error(&text) {
            return Err(Error::Nexus(message));
        }
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or(text);
        return Err(Error::Nexus(match status.as_u16() {
            401 => "Nexus Mods didn't accept this API key. Copy the whole Personal API Key from your Nexus account page and try again.".into(),
            403 => format!("Nexus Mods refused the request: {message}"),
            404 => "Collection not found.".into(),
            429 => "Nexus Mods rate limit reached; try again later.".into(),
            code => format!("Nexus Mods error {code}: {message}"),
        }));
    }
    parse_collection_response(&text)
}

/// Turns a GraphQL response body into a [`Collection`], or the first GraphQL
/// error into a user-facing message.
pub fn parse_collection_response(body: &str) -> Result<Collection> {
    if let Some(message) = graphql_error(body) {
        return Err(Error::Nexus(message));
    }
    let response: Response = serde_json::from_str(body)
        .map_err(|e| Error::Nexus(format!("unexpected response from Nexus Mods: {e}")))?;
    let revision = response
        .data
        .and_then(|d| d.collection_revision)
        .ok_or_else(|| Error::Nexus("Collection not found.".into()))?;
    let collection = revision.collection;
    let game_domain = collection.game.domain_name;

    let mut mods = Vec::new();
    let mut unavailable_file_ids = Vec::new();
    for entry in revision.mod_files {
        let Some(file) = entry.file else {
            unavailable_file_ids.push(entry.file_id);
            continue;
        };
        // The file's own version; the collection's copy of it as a fallback.
        let version = std::iter::once(file.version)
            .chain(entry.version)
            .find(|v| !v.trim().is_empty());
        mods.push(CollectionMod {
            mod_id: file.mod_id,
            file_id: file.file_id,
            name: file
                .r#mod
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| file.name.clone()),
            file_title: file.name,
            file_name: file.uri.filter(|u| !u.is_empty()),
            version,
            size_bytes: file.size_in_bytes.and_then(|s| s.as_u64()),
            optional: entry.optional,
            game_domain: file
                .r#mod
                .map(|m| m.game.domain_name)
                .unwrap_or_else(|| game_domain.clone()),
        });
    }

    Ok(Collection {
        slug: collection.slug,
        name: collection.name,
        author: collection.user.map(|u| u.name).unwrap_or_default(),
        summary: collection.summary.filter(|s| !s.trim().is_empty()),
        revision: revision.revision_number,
        game_domain,
        mods,
        unavailable_file_ids,
    })
}

/// The first entry of a GraphQL `errors` array as a user-facing message.
fn graphql_error(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let error = value.get("errors")?.as_array()?.first()?;
    let message = error
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown error");
    let code = error
        .pointer("/extensions/code")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let lower = message.to_ascii_lowercase();
    Some(match code {
        "NOT_FOUND" if lower.contains("revision") => {
            "This revision of the collection wasn't found. It may have been removed; try the collection's latest revision.".into()
        }
        "NOT_FOUND" => "Collection not found.".into(),
        "ADULT_CONTENT_BLOCKED" => {
            "This collection contains adult content. Allow adult content in your Nexus Mods account settings to use it.".into()
        }
        "UNAUTHENTICATED" | "UNAUTHORIZED" | "FORBIDDEN" => {
            format!("Nexus Mods refused the request: {message}")
        }
        _ if lower.contains("rate limit") || lower.contains("too many requests") => {
            "Nexus Mods rate limit reached; try again later.".into()
        }
        _ => format!("Nexus Mods error: {message}"),
    })
}

// The GraphQL response, as asked for by `QUERY`.

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    collection_revision: Option<RevisionData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionData {
    revision_number: u32,
    collection: CollectionData,
    #[serde(default)]
    mod_files: Vec<RevisionModData>,
}

#[derive(Deserialize)]
struct CollectionData {
    slug: String,
    name: String,
    summary: Option<String>,
    user: Option<UserData>,
    game: GameData,
}

#[derive(Deserialize)]
struct UserData {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GameData {
    domain_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionModData {
    file_id: u32,
    optional: bool,
    #[serde(default)]
    version: Option<String>,
    /// Null when the file was deleted or hidden after the revision was published.
    file: Option<FileData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileData {
    file_id: u32,
    mod_id: u32,
    name: String,
    version: String,
    uri: Option<String>,
    size_in_bytes: Option<BigInt>,
    r#mod: Option<ModData>,
}

#[derive(Deserialize)]
struct ModData {
    name: String,
    game: GameData,
}

/// GraphQL `BigInt`: Nexus sends it as a string, but accept a number too.
#[derive(Deserialize)]
#[serde(untagged)]
enum BigInt {
    Number(u64),
    Text(String),
}

impl BigInt {
    fn as_u64(&self) -> Option<u64> {
        match self {
            BigInt::Number(n) => Some(*n),
            BigInt::Text(s) => s.trim().parse().ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(game: &str, slug: &str, revision: Option<u32>) -> CollectionLink {
        CollectionLink {
            game_domain: game.into(),
            slug: slug.into(),
            revision,
        }
    }

    #[test]
    fn parses_nxm_collection_links() {
        assert_eq!(
            parse_collection_link("nxm://cyberpunk2077/collections/ewdjcz/revisions/76").unwrap(),
            link("cyberpunk2077", "ewdjcz", Some(76))
        );
        assert_eq!(
            parse_collection_link("nxm://Cyberpunk2077/collections/ewdjcz").unwrap(),
            link("cyberpunk2077", "ewdjcz", None)
        );
        assert!(parse_collection_link("nxm://cyberpunk2077/mods/107/files/1").is_err());
        assert!(
            parse_collection_link("nxm://cyberpunk2077/collections/ewdjcz/revisions/x").is_err()
        );
    }

    #[test]
    fn parses_collection_page_urls() {
        assert_eq!(
            parse_collection_link(
                "https://www.nexusmods.com/games/cyberpunk2077/collections/ewdjcz"
            )
            .unwrap(),
            link("cyberpunk2077", "ewdjcz", None)
        );
        assert_eq!(
            parse_collection_link(
                "https://www.nexusmods.com/games/cyberpunk2077/collections/ewdjcz/revisions/70?tab=mods"
            )
            .unwrap(),
            link("cyberpunk2077", "ewdjcz", Some(70))
        );
        assert_eq!(
            parse_collection_link(
                "https://www.nexusmods.com/games/cyberpunk2077/collections/ewdjcz/mods"
            )
            .unwrap(),
            link("cyberpunk2077", "ewdjcz", None)
        );
        assert_eq!(
            parse_collection_link(
                " https://next.nexusmods.com/skyrimspecialedition/collections/abc123 "
            )
            .unwrap(),
            link("skyrimspecialedition", "abc123", None)
        );
        assert!(parse_collection_link("https://example.com/games/x/collections/abc").is_err());
        assert!(
            parse_collection_link("https://www.nexusmods.com/games/cyberpunk2077/mods/107")
                .is_err()
        );
        assert!(parse_collection_link("not a link").is_err());
    }

    /// Trimmed from a live response for `ewdjcz` (2077 Immersive++), revision 76.
    const RESPONSE: &str = r#"{"data":{"collectionRevision":{"revisionNumber":76,"collection":{"slug":"ewdjcz","name":"2077 Immersive++","summary":"A fuss-free collection that delivers a more immersive, polished, flexible, and fun experience.","user":{"name":"XerDuke"},"game":{"domainName":"cyberpunk2077"}},"modFiles":[
        {"fileId":120421,"optional":false,"version":"0.5.31","file":{"fileId":120421,"modId":1511,"name":"redscript","version":"0.5.31","uri":"redscript-1511-0-5-31-1757113888.zip","sizeInBytes":"948910","mod":{"name":"redscript","game":{"domainName":"cyberpunk2077"}}}},
        {"fileId":48188,"optional":false,"version":"1.0.0a","file":{"fileId":48188,"modId":9031,"name":"Preem Shores","version":"1.0.0a","uri":"Preem Shores-9031-1-0-0-1693000434.zip","sizeInBytes":"5358527","mod":{"name":"Preem Shores (Preem Water Addon - Trash Begone)","game":{"domainName":"cyberpunk2077"}}}},
        {"fileId":140722,"optional":true,"version":"1.0","file":{"fileId":140722,"modId":28217,"name":"Veegee SuSa Hair - CCXL","version":"1.0","uri":"Veegee SuSa Hair - CCXL-28217-1-0-1774723118.zip","sizeInBytes":"2691139","mod":{"name":"Veegee Susa Hair - CCXL","game":{"domainName":"cyberpunk2077"}}}},
        {"fileId":99,"optional":false,"version":"1.0","file":null}
    ]}}}"#;

    #[test]
    fn parses_collection_response() {
        let collection = parse_collection_response(RESPONSE).unwrap();
        assert_eq!(collection.slug, "ewdjcz");
        assert_eq!(collection.name, "2077 Immersive++");
        assert_eq!(collection.author, "XerDuke");
        assert_eq!(collection.revision, 76);
        assert_eq!(collection.game_domain, "cyberpunk2077");
        assert!(collection.summary.is_some());
        assert_eq!(collection.mods.len(), 3);
        assert_eq!(collection.unavailable_file_ids, vec![99]);
        assert_eq!(
            collection.mods[1],
            CollectionMod {
                mod_id: 9031,
                file_id: 48188,
                name: "Preem Shores (Preem Water Addon - Trash Begone)".into(),
                file_title: "Preem Shores".into(),
                file_name: Some("Preem Shores-9031-1-0-0-1693000434.zip".into()),
                version: Some("1.0.0a".into()),
                size_bytes: Some(5358527),
                optional: false,
                game_domain: "cyberpunk2077".into(),
            }
        );
        assert!(collection.mods[2].optional);
    }

    /// Talks to the real API: `cargo test -p mm-core collections -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_live_collection() {
        let latest =
            fetch_collection(None, &link("cyberpunk2077", "ewdjcz", None), "0.0.0").unwrap();
        assert!(latest.revision >= 76 && !latest.mods.is_empty());
        let pinned =
            fetch_collection(None, &link("cyberpunk2077", "devnx1", Some(40)), "0.0.0").unwrap();
        assert_eq!(pinned.revision, 40);
        let missing = fetch_collection(None, &link("cyberpunk2077", "zzzzzz", None), "0.0.0");
        assert_eq!(missing.unwrap_err().to_string(), "Collection not found.");
    }

    #[test]
    fn maps_graphql_errors() {
        let not_found = r#"{"errors":[{"message":"Collection not found","path":["collectionRevision"],"extensions":{"code":"NOT_FOUND","detail":"Collection not found"}}],"data":null}"#;
        assert_eq!(
            parse_collection_response(not_found)
                .unwrap_err()
                .to_string(),
            "Collection not found."
        );
        let no_revision = r#"{"errors":[{"message":"CollectionRevision not found","extensions":{"code":"NOT_FOUND"}}],"data":null}"#;
        assert!(
            parse_collection_response(no_revision)
                .unwrap_err()
                .to_string()
                .contains("revision")
        );
        let adult = r#"{"errors":[{"message":"Adult content blocked","extensions":{"code":"ADULT_CONTENT_BLOCKED"}}],"data":null}"#;
        assert!(
            parse_collection_response(adult)
                .unwrap_err()
                .to_string()
                .contains("adult content")
        );
        assert!(parse_collection_response(r#"{"data":{"collectionRevision":null}}"#).is_err());
    }
}
