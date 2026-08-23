//! Torrent search across remote providers (apibay and Torznab indexers such
//! as Jackett/Prowlarr).
//!
//! The UI kicks off [`run_search`] with the providers built from config and
//! awaits a single [`SearchResponse`] on the channel. Every provider runs in
//! its own task against one shared HTTP client, so a slow or broken indexer
//! never blocks the others — its failure just lands in
//! [`SearchResponse::errors`].

mod apibay;
mod torznab;

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::config::TorznabConfig;

/// Per-provider result cap. Indexers can return thousands of rows and the TUI
/// only ever shows the top slice, so bounding this keeps a pathological
/// response from eating memory.
const MAX_RESULTS_PER_PROVIDER: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub magnet: String,
    pub size_bytes: u64,
    pub seeders: u32,
    pub leechers: u32,
    /// Short display name, e.g. "TPB" or the torznab entry name from config.
    pub provider: String,
}

#[derive(Debug, Clone, Default)]
pub struct SearchResponse {
    pub query: String,
    /// Merged across providers, deduped by magnet, sorted by seeders desc.
    pub results: Vec<SearchResult>,
    /// (provider name, error message) for providers that failed.
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub enum Provider {
    Apibay,
    Torznab(TorznabConfig),
}

impl Provider {
    /// Display name attached to every [`SearchResult`] this provider returns.
    pub fn name(&self) -> &str {
        match self {
            Provider::Apibay => apibay::PROVIDER_NAME,
            Provider::Torznab(cfg) => &cfg.name,
        }
    }

    pub async fn search(&self, client: &reqwest::Client, query: &str) -> Result<Vec<SearchResult>> {
        match self {
            Provider::Apibay => apibay::search(client, query).await,
            Provider::Torznab(cfg) => torznab::search(client, cfg, query).await,
        }
    }
}

/// Fan `query` out to every provider, then send exactly one merged
/// [`SearchResponse`]. Provider failures are collected into `errors` and never
/// abort the remaining providers.
pub async fn run_search(providers: Vec<Provider>, query: String, tx: mpsc::Sender<SearchResponse>) {
    let mut response = SearchResponse {
        query: query.clone(),
        ..Default::default()
    };

    if providers.is_empty() || query.trim().is_empty() {
        let _ = tx.send(response).await;
        return;
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("torrenttui/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            response.errors.push((
                "search".to_string(),
                format!("HTTP client init failed: {e}"),
            ));
            let _ = tx.send(response).await;
            return;
        }
    };

    let mut set: JoinSet<(String, Result<Vec<SearchResult>>)> = JoinSet::new();
    for provider in providers {
        let client = client.clone();
        let query = query.clone();
        set.spawn(async move {
            let name = provider.name().to_string();
            let res = provider.search(&client, &query).await;
            (name, res)
        });
    }

    let mut outcomes = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) => response
                .errors
                .push(("search".to_string(), format!("provider task failed: {e}"))),
        }
    }

    let (results, errors) = merge_results(outcomes);
    response.results = results;
    response.errors.extend(errors);
    let _ = tx.send(response).await;
}

/// Merge per-provider outcomes into the final result list and error list.
/// Sorted by seeders descending *before* dedup so that when two providers
/// return the same magnet, the copy with the higher seeder count survives.
fn merge_results(
    outcomes: Vec<(String, Result<Vec<SearchResult>>)>,
) -> (Vec<SearchResult>, Vec<(String, String)>) {
    let mut results = Vec::new();
    let mut errors = Vec::new();
    for (name, outcome) in outcomes {
        match outcome {
            Ok(mut hits) => results.append(&mut hits),
            Err(e) => errors.push((name, format!("{e:#}"))),
        }
    }
    results.sort_by_key(|r| std::cmp::Reverse(r.seeders));
    let mut seen = HashSet::new();
    results.retain(|r| seen.insert(r.magnet.clone()));
    (results, errors)
}

/// Percent-encode per RFC 3986 (unreserved characters pass through). Used for
/// the `dn`/`tr` magnet params, which reqwest's query encoding never touches.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Parse a numeric string, treating malformed input as 0 rather than an
/// error — indexers occasionally emit junk in numeric fields.
fn parse_u64(s: &str) -> u64 {
    s.trim().parse().unwrap_or(0)
}

fn parse_u32(s: &str) -> u32 {
    s.trim().parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(magnet: &str, seeders: u32, provider: &str) -> SearchResult {
        SearchResult {
            title: format!("title of {magnet}"),
            magnet: magnet.to_string(),
            size_bytes: 1024,
            seeders,
            leechers: 0,
            provider: provider.to_string(),
        }
    }

    #[test]
    fn merge_sorts_by_seeders_desc() {
        let outcomes = vec![
            (
                "TPB".to_string(),
                Ok(vec![result("magnet:?xt=urn:btih:aaa", 5, "TPB")]),
            ),
            (
                "jackett".to_string(),
                Ok(vec![result("magnet:?xt=urn:btih:bbb", 50, "jackett")]),
            ),
        ];
        let (results, errors) = merge_results(outcomes);
        assert!(errors.is_empty());
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].seeders, 50);
        assert_eq!(results[1].seeders, 5);
    }

    #[test]
    fn merge_dedups_identical_magnets_keeping_highest_seeders() {
        let dup = "magnet:?xt=urn:btih:ccc";
        let outcomes = vec![
            ("TPB".to_string(), Ok(vec![result(dup, 7, "TPB")])),
            ("jackett".to_string(), Ok(vec![result(dup, 42, "jackett")])),
        ];
        let (results, _) = merge_results(outcomes);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].seeders, 42);
        assert_eq!(results[0].provider, "jackett");
    }

    #[test]
    fn merge_collects_errors_without_dropping_other_results() {
        let outcomes = vec![
            (
                "TPB".to_string(),
                Err(anyhow::anyhow!("connection timed out")),
            ),
            (
                "jackett".to_string(),
                Ok(vec![result("magnet:?xt=urn:btih:ddd", 3, "jackett")]),
            ),
        ];
        let (results, errors) = merge_results(outcomes);
        assert_eq!(results.len(), 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "TPB");
        assert!(errors[0].1.contains("connection timed out"));
    }

    #[test]
    fn percent_encode_encodes_reserved_bytes() {
        assert_eq!(percent_encode("plain-Name_1.0~x"), "plain-Name_1.0~x");
        assert_eq!(
            percent_encode("udp://tracker.openbittorrent.com:6969/announce"),
            "udp%3A%2F%2Ftracker.openbittorrent.com%3A6969%2Fannounce"
        );
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn parse_numbers_tolerate_junk() {
        assert_eq!(parse_u64("123"), 123);
        assert_eq!(parse_u64(" 42 "), 42);
        assert_eq!(parse_u64(""), 0);
        assert_eq!(parse_u64("not-a-number"), 0);
        assert_eq!(parse_u32("17"), 17);
        assert_eq!(parse_u32("1.5"), 0);
    }
}
