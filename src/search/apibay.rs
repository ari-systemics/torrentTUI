//! apibay.org provider (The Pirate Bay's official API). Returns JSON; only
//! the info hash is provided, so the magnet (with `dn` and public trackers)
//! is constructed locally.

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{parse_u32, parse_u64, percent_encode, SearchResult, MAX_RESULTS_PER_PROVIDER};

pub(super) const PROVIDER_NAME: &str = "TPB";

const APIBAY_URL: &str = "https://apibay.org/q.php";

/// Well-known public trackers appended to every constructed magnet — apibay
/// gives us only the info hash, and a trackerless magnet relies on DHT alone.
const TRACKERS: [&str; 4] = [
    "udp://tracker.openbittorrent.com:6969/announce",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker.torrent.eu.org:451/announce",
];

/// apibay rows carry many more fields than this; serde ignores the rest.
/// Numbers arrive as strings.
#[derive(Debug, Deserialize)]
struct ApibayEntry {
    id: String,
    name: String,
    info_hash: String,
    size: String,
    seeders: String,
    leechers: String,
}

pub(super) async fn search(client: &reqwest::Client, query: &str) -> Result<Vec<SearchResult>> {
    let body = client
        .get(APIBAY_URL)
        .query(&[("q", query)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    parse_apibay(&body)
}

fn parse_apibay(json: &str) -> Result<Vec<SearchResult>> {
    let entries: Vec<ApibayEntry> =
        serde_json::from_str(json).context("invalid apibay response")?;
    let results = entries
        .into_iter()
        .filter_map(|entry| {
            // apibay signals an empty result set with a sentinel row (id "0",
            // name "No results returned") instead of an empty array.
            if entry.id == "0" || entry.name == "No results returned" || entry.info_hash.is_empty()
            {
                return None;
            }
            Some(SearchResult {
                magnet: build_magnet(&entry.info_hash, &entry.name),
                title: entry.name,
                size_bytes: parse_u64(&entry.size),
                seeders: parse_u32(&entry.seeders),
                leechers: parse_u32(&entry.leechers),
                provider: PROVIDER_NAME.to_string(),
            })
        })
        .take(MAX_RESULTS_PER_PROVIDER)
        .collect();
    Ok(results)
}

fn build_magnet(info_hash: &str, name: &str) -> String {
    let mut magnet = format!(
        "magnet:?xt=urn:btih:{}&dn={}",
        info_hash,
        percent_encode(name)
    );
    for tracker in TRACKERS {
        magnet.push_str("&tr=");
        magnet.push_str(&percent_encode(tracker));
    }
    magnet
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
        {
            "id": "7043651",
            "name": "Ubuntu 24.04.1 Desktop amd64",
            "info_hash": "AB2D21C0F4372D18A76A4A1A1E7BDB7FB5B2A001",
            "leechers": "12",
            "seeders": "350",
            "num_files": "1",
            "size": "6071949312",
            "username": "linuxfan",
            "added": "1727000000",
            "status": "vip",
            "category": "300",
            "imdb": ""
        },
        {
            "id": "7043660",
            "name": "Debian 12.7 netinst & extras",
            "info_hash": "00112233445566778899aabbccddeeff00112233",
            "leechers": "junk",
            "seeders": "88",
            "num_files": "3",
            "size": "bad-size",
            "username": "someone",
            "added": "1727000100",
            "status": "member",
            "category": "300",
            "imdb": ""
        },
        {
            "id": "0",
            "name": "No results returned",
            "info_hash": "0000000000000000000000000000000000000000",
            "leechers": "0",
            "seeders": "0",
            "num_files": "0",
            "size": "0",
            "username": "",
            "added": "0",
            "status": "member",
            "category": "0",
            "imdb": ""
        }
    ]"#;

    #[test]
    fn parses_entries_and_skips_sentinel() {
        let results = parse_apibay(FIXTURE).unwrap();
        assert_eq!(results.len(), 2);

        let first = &results[0];
        assert_eq!(first.title, "Ubuntu 24.04.1 Desktop amd64");
        assert_eq!(first.size_bytes, 6071949312);
        assert_eq!(first.seeders, 350);
        assert_eq!(first.leechers, 12);
        assert_eq!(first.provider, "TPB");

        // Malformed numeric strings degrade to 0, not to a dropped row.
        let second = &results[1];
        assert_eq!(second.size_bytes, 0);
        assert_eq!(second.leechers, 0);
        assert_eq!(second.seeders, 88);
    }

    #[test]
    fn magnet_has_btih_dn_and_trackers() {
        let results = parse_apibay(FIXTURE).unwrap();
        let magnet = &results[0].magnet;
        assert!(
            magnet.starts_with("magnet:?xt=urn:btih:AB2D21C0F4372D18A76A4A1A1E7BDB7FB5B2A001&dn=")
        );
        assert!(magnet.contains("Ubuntu%2024.04.1%20Desktop%20amd64"));
        assert!(magnet.contains("&tr=udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce"));
        assert!(magnet.contains("&tr=udp%3A%2F%2Ftracker.opentrackr.org%3A1337%2Fannounce"));
        assert_eq!(magnet.matches("&tr=").count(), TRACKERS.len());

        // Spaces and ampersands in the name are encoded, not raw.
        let magnet2 = &results[1].magnet;
        assert!(magnet2.contains("dn=Debian%2012.7%20netinst%20%26%20extras"));
    }

    #[test]
    fn empty_array_is_not_an_error() {
        assert!(parse_apibay("[]").unwrap().is_empty());
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_apibay("not json").is_err());
    }

    #[test]
    fn result_count_is_capped() {
        let entry =
            r#"{"id":"1","name":"x","info_hash":"abc","leechers":"0","seeders":"1","size":"1"}"#;
        let json = format!(
            "[{}]",
            (0..MAX_RESULTS_PER_PROVIDER + 50)
                .map(|_| entry)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(parse_apibay(&json).unwrap().len(), MAX_RESULTS_PER_PROVIDER);
    }
}
