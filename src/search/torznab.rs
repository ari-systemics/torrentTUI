//! Torznab provider (Jackett/Prowlarr). The indexer speaks an RSS dialect:
//! items carry their metadata in `<torznab:attr name="..." value="..."/>`
//! elements. Only items with a magnet link are kept — the app's add path
//! accepts magnets or local .torrent files only, so an http(s) .torrent URL
//! would produce a row the user cannot actually add.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::TorznabConfig;

use super::{parse_u32, parse_u64, SearchResult, MAX_RESULTS_PER_PROVIDER};

#[derive(Debug, Deserialize)]
struct Rss {
    channel: Channel,
}

#[derive(Debug, Deserialize)]
struct Channel {
    #[serde(default)]
    item: Vec<Item>,
}

#[derive(Debug, Deserialize)]
struct Item {
    title: Option<String>,
    /// Bytes. Kept as text because indexers occasionally emit junk in numeric
    /// fields, which should degrade to 0 rather than fail the whole feed.
    size: Option<String>,
    link: Option<String>,
    enclosure: Option<Enclosure>,
    /// quick-xml's serde layer matches element local names, so `torznab:attr`
    /// is found via the plain "attr" rename — no namespace handling needed.
    #[serde(rename = "attr", default)]
    attrs: Vec<Attr>,
}

#[derive(Debug, Deserialize)]
struct Enclosure {
    #[serde(rename = "@url")]
    url: String,
}

#[derive(Debug, Deserialize)]
struct Attr {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "@value")]
    value: String,
}

pub(super) async fn search(
    client: &reqwest::Client,
    cfg: &TorznabConfig,
    query: &str,
) -> Result<Vec<SearchResult>> {
    let body = client
        .get(&cfg.url)
        .query(&[("t", "search"), ("q", query), ("apikey", &cfg.api_key)])
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    parse_torznab(&body, &cfg.name)
}

fn parse_torznab(xml: &str, provider: &str) -> Result<Vec<SearchResult>> {
    let rss: Rss = quick_xml::de::from_str(xml).context("invalid torznab response")?;
    let results = rss
        .channel
        .item
        .into_iter()
        .filter_map(|item| {
            let magnet = extract_magnet(&item)?;
            let attr = |name: &str| {
                item.attrs
                    .iter()
                    .find(|a| a.name == name)
                    .map(|a| a.value.as_str())
            };
            let seeders = attr("seeders").map(parse_u32).unwrap_or(0);
            // "leechers" is the torznab standard; some indexers only report
            // the combined "peers" count, which is the closer approximation.
            let leechers = attr("leechers")
                .or_else(|| attr("peers"))
                .map(parse_u32)
                .unwrap_or(0);
            Some(SearchResult {
                title: item.title.unwrap_or_default().trim().to_string(),
                magnet,
                size_bytes: item.size.as_deref().map(parse_u64).unwrap_or(0),
                seeders,
                leechers,
                provider: provider.to_string(),
            })
        })
        .take(MAX_RESULTS_PER_PROVIDER)
        .collect();
    Ok(results)
}

/// Magnet preference order: the explicit `magneturl` torznab attr, then a
/// `magnet:` `<link>`, then a `magnet:` enclosure URL. Items with only an
/// http(s) .torrent link return None and are skipped (see module docs).
fn extract_magnet(item: &Item) -> Option<String> {
    let magneturl = item
        .attrs
        .iter()
        .find(|a| a.name == "magneturl")
        .map(|a| a.value.trim());
    if let Some(v) = magneturl {
        if v.starts_with("magnet:") {
            return Some(v.to_string());
        }
    }
    let link = item.link.as_deref().map(str::trim);
    let enclosure = item.enclosure.as_ref().map(|e| e.url.trim());
    for candidate in [link, enclosure].into_iter().flatten() {
        if candidate.starts_with("magnet:") {
            return Some(candidate.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:torznab="http://torznab.com/schemas/2015/feed">
  <channel>
    <title>jackett-all</title>
    <item>
      <title>Arch Linux 2024.09.01 x86_64</title>
      <size>1234567890</size>
      <link>http://127.0.0.1:9117/dl/archlinux.torrent</link>
      <torznab:attr name="magneturl" value="magnet:?xt=urn:btih:AAAA1111BBBB2222CCCC3333DDDD4444EEEE5555&amp;dn=Arch%20Linux"/>
      <torznab:attr name="seeders" value="120"/>
      <torznab:attr name="leechers" value="9"/>
    </item>
    <item>
      <title>Fedora 40 Workstation</title>
      <size>2345678901</size>
      <link>magnet:?xt=urn:btih:FFFF6666FFFF6666FFFF6666FFFF6666FFFF6666&amp;dn=Fedora%2040</link>
      <enclosure url="magnet:?xt=urn:btih:FFFF6666FFFF6666FFFF6666FFFF6666FFFF6666&amp;dn=Fedora%2040" length="2345678901" type="application/x-bittorrent"/>
      <torznab:attr name="seeders" value="45"/>
      <torznab:attr name="peers" value="50"/>
    </item>
    <item>
      <title>Some http-only release</title>
      <size>3456789012</size>
      <link>http://127.0.0.1:9117/dl/httponly.torrent</link>
      <enclosure url="http://127.0.0.1:9117/dl/httponly.torrent" length="3456789012" type="application/x-bittorrent"/>
      <torznab:attr name="seeders" value="3"/>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_items_and_extracts_magnets() {
        let results = parse_torznab(FIXTURE, "jackett-all").unwrap();
        // The http-only item has no magnet anywhere, so it is skipped.
        assert_eq!(results.len(), 2);

        let first = &results[0];
        assert_eq!(first.title, "Arch Linux 2024.09.01 x86_64");
        assert_eq!(
            first.magnet,
            "magnet:?xt=urn:btih:AAAA1111BBBB2222CCCC3333DDDD4444EEEE5555&dn=Arch%20Linux"
        );
        assert_eq!(first.size_bytes, 1234567890);
        assert_eq!(first.seeders, 120);
        assert_eq!(first.leechers, 9);
        assert_eq!(first.provider, "jackett-all");

        // Second item: magnet comes from <link>, leechers falls back to peers.
        let second = &results[1];
        assert_eq!(
            second.magnet,
            "magnet:?xt=urn:btih:FFFF6666FFFF6666FFFF6666FFFF6666FFFF6666&dn=Fedora%2040"
        );
        assert_eq!(second.seeders, 45);
        assert_eq!(second.leechers, 50);
    }

    #[test]
    fn missing_magnet_attrs_still_parse() {
        let xml = r#"<rss version="2.0"><channel><item>
            <title>x</title>
            <link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link>
        </item></channel></rss>"#;
        let results = parse_torznab(xml, "p").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].seeders, 0);
        assert_eq!(results[0].leechers, 0);
        assert_eq!(results[0].size_bytes, 0);
    }

    #[test]
    fn malformed_numeric_fields_degrade_to_zero() {
        let xml = r#"<rss version="2.0"><channel><item>
            <title>x</title>
            <size>huge</size>
            <link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567</link>
            <torznab:attr name="seeders" value="many"/>
        </item></channel></rss>"#;
        let results = parse_torznab(xml, "p").unwrap();
        assert_eq!(results[0].size_bytes, 0);
        assert_eq!(results[0].seeders, 0);
    }

    #[test]
    fn empty_channel_is_not_an_error() {
        let xml = r#"<rss version="2.0"><channel><title>empty</title></channel></rss>"#;
        assert!(parse_torznab(xml, "p").unwrap().is_empty());
    }

    #[test]
    fn invalid_xml_is_an_error() {
        assert!(parse_torznab("not xml at all", "p").is_err());
    }
}
