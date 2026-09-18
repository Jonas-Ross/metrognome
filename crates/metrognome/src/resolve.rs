//! Resolving an artist and title to an iTunes store track with a preview URL.
//!
//! selecta only has Music.app persistent IDs, which are not store IDs, so
//! artist/title search is the primary path and matching is necessarily fuzzy:
//! "feat." spellings differ, releases carry "(Remastered 2011)", and a remix
//! shares its title with the original while sharing neither its tempo nor its
//! key. Matching is therefore split from HTTP — [`pick_best`] is a pure
//! function over parsed results, and the tests drive it from recorded fixtures.
//!
//! Every response includes the matched track's own metadata and a match score,
//! so a wrong match is visible to the caller rather than silently believed.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::ratelimit::RateLimiter;
use crate::types::TrackMatch;

/// How many search results to consider.
///
/// Enough to get past a run of remixes and karaoke covers to the original;
/// beyond this the extra candidates are noise and cost response size.
const SEARCH_LIMIT: u32 = 12;

/// At or above this score the match is treated as solid.
///
/// Calibrated so that a title differing only by a neutral qualifier still
/// clears it, while a remix or a different track does not.
pub const MATCH_CONFIDENT_AT_OR_ABOVE: f32 = 0.85;

/// Qualifiers that name a different master of the *same performance*.
///
/// Stripping these makes "Around the World (Radio Edit)" match "Around the
/// World" without also making "Around the World (Deep Dish Remix)" match it —
/// a remix is a different recording with its own tempo and often its own key,
/// and matching one to the other would poison the feature data.
const NEUTRAL_QUALIFIERS: [&str; 14] = [
    "original mix",
    "album version",
    "single version",
    "radio edit",
    "radio mix",
    "remastered",
    "remaster",
    "explicit",
    "clean",
    "bonus track",
    "deluxe edition",
    "deluxe",
    "mono",
    "stereo",
];

/// One track as returned by the iTunes Search or Lookup API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItunesTrack {
    /// Store track ID.
    pub track_id: Option<i64>,
    /// Track title.
    pub track_name: Option<String>,
    /// Primary artist name.
    pub artist_name: Option<String>,
    /// Album name.
    pub collection_name: Option<String>,
    /// ISO-8601 release timestamp.
    pub release_date: Option<String>,
    /// Apple's own genre label.
    pub primary_genre_name: Option<String>,
    /// URL of the 30-second preview clip.
    pub preview_url: Option<String>,
    /// Track duration in milliseconds.
    pub track_time_millis: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ItunesResponse {
    #[serde(default)]
    results: Vec<ItunesTrack>,
}

/// Parse a raw iTunes API response body.
///
/// Public so fixtures can be replayed without a network round trip.
pub fn parse_response(body: &str) -> Result<Vec<ItunesTrack>> {
    serde_json::from_str::<ItunesResponse>(body)
        .map(|r| r.results)
        .map_err(|e| Error::Http(format!("malformed itunes response: {e}")))
}

/// Lowercase, fold `&` to `and`, drop punctuation, collapse whitespace.
fn normalize(s: &str) -> String {
    // Expand first so the space-collapsing below handles the boundaries; doing
    // it inline means tracking whether a separator is owed on both sides.
    let expanded = s.replace('&', " and ");
    let mut out = String::with_capacity(expanded.len());
    let mut last_space = true;
    for ch in expanded.chars() {
        let mapped = match ch {
            c if c.is_alphanumeric() => c.to_lowercase().next().unwrap_or(c),
            _ => ' ',
        };
        if mapped == ' ' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(mapped);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// Remove a trailing "feat. …" / "featuring …" / "ft. …" clause.
///
/// Credit formatting is the single most common cosmetic difference between a
/// library's metadata and the store's, and it is never a different recording.
fn strip_features(s: &str) -> String {
    let n = normalize(s);
    for marker in [" feat ", " featuring ", " ft ", " with "] {
        if let Some(i) = n.find(marker) {
            return n[..i].trim().to_string();
        }
    }
    n
}

/// Split a title into its core and its parenthesized qualifier.
fn split_qualifier(title: &str) -> (String, String) {
    let mut core = String::new();
    let mut qual = String::new();
    let mut depth = 0i32;
    for ch in title.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth > 0 => qual.push(ch),
            _ => core.push(ch),
        }
    }
    let mut qual = normalize(&qual);
    for neutral in NEUTRAL_QUALIFIERS {
        qual = qual.replace(neutral, " ");
    }
    // A bare year is a reissue marker, not a different recording.
    qual = qual
        .split_whitespace()
        .filter(|t| !(t.len() == 4 && t.chars().all(|c| c.is_ascii_digit())))
        .collect::<Vec<_>>()
        .join(" ");
    (strip_features(&core), qual.trim().to_string())
}

/// Levenshtein distance as a 0-1 similarity.
fn lev_ratio(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let dist = prev[b.len()] as f32;
    1.0 - dist / a.len().max(b.len()) as f32
}

/// Jaccard similarity over whitespace tokens.
///
/// Complements edit distance: it is insensitive to word order, which matters
/// because artist credits get reordered ("Above & Beyond" vs "Beyond, Above").
fn token_ratio(a: &str, b: &str) -> f32 {
    let at: std::collections::BTreeSet<&str> = a.split_whitespace().collect();
    let bt: std::collections::BTreeSet<&str> = b.split_whitespace().collect();
    if at.is_empty() && bt.is_empty() {
        return 1.0;
    }
    let inter = at.intersection(&bt).count() as f32;
    let union = at.union(&bt).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    0.5 * lev_ratio(a, b) + 0.5 * token_ratio(a, b)
}

/// Weight of the title in the overall match score.
///
/// Titles carry more information than artists: an artist's catalogue has many
/// tracks, but a title plus roughly the right artist is nearly always unique.
const TITLE_WEIGHT: f32 = 0.65;

/// How much of the title score comes from the parenthesized qualifier.
///
/// Small but not zero. It is what keeps a remix from tying with the original,
/// while still letting the original win if nothing better is on offer.
const QUALIFIER_WEIGHT: f32 = 0.25;

/// Ceiling on the title score for a candidate whose title is not character-for
/// -character what was asked for.
///
/// Without it "Around the World" and "Around the World (Radio Edit)" tie for a
/// query of "Around the World", because the qualifier is a neutral one and gets
/// stripped from both sides. The gap is small on purpose: it settles a tie, it
/// does not outrank a genuinely better match.
const INEXACT_TITLE_CEILING: f32 = 0.97;

/// Score one candidate against the query. 0-1.
pub fn score_match(query_artist: &str, query_title: &str, cand: &ItunesTrack) -> f32 {
    let (q_core, q_qual) = split_qualifier(query_title);
    let cand_title = cand.track_name.as_deref().unwrap_or("");
    let (c_core, c_qual) = split_qualifier(cand_title);
    let mut title = (1.0 - QUALIFIER_WEIGHT) * similarity(&q_core, &c_core)
        + QUALIFIER_WEIGHT * similarity(&q_qual, &c_qual);
    if normalize(query_title) != normalize(cand_title) {
        title *= INEXACT_TITLE_CEILING;
    }

    let q_artist = strip_features(query_artist);
    let c_artist = strip_features(cand.artist_name.as_deref().unwrap_or(""));
    let mut artist = similarity(&q_artist, &c_artist);
    // A compilation credits "Various Artists" or adds collaborators. If every
    // word of the query artist appears in the candidate's, that is a credit
    // difference rather than a different act.
    if !q_artist.is_empty()
        && q_artist
            .split_whitespace()
            .all(|t| c_artist.split_whitespace().any(|u| u == t))
    {
        artist = artist.max(0.9);
    }

    TITLE_WEIGHT * title + (1.0 - TITLE_WEIGHT) * artist
}

/// Choose the best candidate for a query, ignoring any without a preview.
///
/// A track with no `previewUrl` cannot be analyzed, so it is not a match no
/// matter how well its metadata scores — returning one would produce a
/// confident-looking result with nothing behind it.
pub fn pick_best(artist: &str, title: &str, candidates: &[ItunesTrack]) -> Option<TrackMatch> {
    candidates
        .iter()
        .filter(|c| c.track_id.is_some() && c.preview_url.is_some())
        .map(|c| (score_match(artist, title, c), c))
        .max_by(|a, b| {
            a.0.total_cmp(&b.0)
                // Equal scores: prefer the plainest title, measured on the raw
                // qualifier rather than the neutral-stripped one. A store full
                // of reissues offers the same recording several times, and the
                // one with no bracket is the one the caller meant.
                .then_with(|| raw_qualifier_len(b.1).cmp(&raw_qualifier_len(a.1)))
                // Still tied: order by ID so the same input always resolves the
                // same way.
                .then_with(|| b.1.track_id.cmp(&a.1.track_id))
        })
        .map(|(score, c)| to_match(c, score))
}

fn raw_qualifier_len(c: &ItunesTrack) -> usize {
    let title = c.track_name.as_deref().unwrap_or("");
    let (core, _) = {
        let mut core = String::new();
        let mut depth = 0i32;
        for ch in title.chars() {
            match ch {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = (depth - 1).max(0),
                _ if depth == 0 => core.push(ch),
                _ => {}
            }
        }
        (core, ())
    };
    title.len().saturating_sub(core.trim().len())
}

fn to_match(c: &ItunesTrack, score: f32) -> TrackMatch {
    TrackMatch {
        track_id: c.track_id.unwrap_or_default(),
        artist: c.artist_name.clone().unwrap_or_default(),
        title: c.track_name.clone().unwrap_or_default(),
        album: c.collection_name.clone(),
        release_date: c.release_date.clone(),
        genre: c.primary_genre_name.clone(),
        preview_url: c.preview_url.clone(),
        duration_ms: c.track_time_millis,
        match_score: (score * 1000.0).round() / 1000.0,
        uncertain: score < MATCH_CONFIDENT_AT_OR_ABOVE,
    }
}

/// Client for the iTunes Search and Lookup endpoints.
pub struct Resolver {
    client: reqwest::Client,
    limiter: RateLimiter,
    base_url: String,
}

impl Resolver {
    /// New resolver against the public iTunes API.
    pub fn new(client: reqwest::Client, limiter: RateLimiter) -> Self {
        Resolver {
            client,
            limiter,
            base_url: "https://itunes.apple.com".into(),
        }
    }

    /// Point the resolver at a different origin (used by integration tests).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    async fn get(&self, url: &str) -> Result<String> {
        self.limiter.acquire().await;
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(Error::Http(format!("GET {url}: status {status}")));
        }
        resp.text()
            .await
            .map_err(|e| Error::Http(format!("read body {url}: {e}")))
    }

    /// Resolve by store track ID.
    pub async fn lookup(&self, track_id: i64) -> Result<TrackMatch> {
        let url = format!("{}/lookup?id={track_id}&entity=song", self.base_url);
        let results = parse_response(&self.get(&url).await?)?;
        results
            .iter()
            .find(|c| c.track_id == Some(track_id))
            // An ID is an exact identification, so the score is 1.0 by
            // definition — there is no fuzziness to report.
            .map(|c| to_match(c, 1.0))
            .ok_or(Error::NoPreview { track_id })
            .and_then(|m| {
                if m.preview_url.is_some() {
                    Ok(m)
                } else {
                    Err(Error::NoPreview { track_id })
                }
            })
    }

    /// Resolve by artist and title.
    pub async fn search(&self, artist: &str, title: &str) -> Result<TrackMatch> {
        let term = urlencode(&format!("{artist} {title}"));
        let url = format!(
            "{}/search?term={term}&media=music&entity=song&limit={SEARCH_LIMIT}",
            self.base_url
        );
        let results = parse_response(&self.get(&url).await?)?;
        pick_best(artist, title, &results).ok_or_else(|| Error::NoMatch {
            artist: artist.to_string(),
            title: title.to_string(),
        })
    }
}

/// Percent-encode a query term.
///
/// Hand-rolled rather than pulling in a URL crate for one call site: the only
/// thing being encoded is a search term, and the unreserved set is short.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_FIXTURE: &str = include_str!("../tests/fixtures/search_around_the_world.json");
    const REMIX_FIXTURE: &str = include_str!("../tests/fixtures/search_remix_heavy.json");
    const EMPTY_FIXTURE: &str = include_str!("../tests/fixtures/search_empty.json");

    #[test]
    fn normalizes_punctuation_case_and_ampersands() {
        assert_eq!(normalize("Sandstorm!"), "sandstorm");
        assert_eq!(normalize("Above & Beyond"), "above and beyond");
        assert_eq!(normalize("A&B"), "a and b");
        assert_eq!(normalize("  Spaced   Out  "), "spaced out");
    }

    #[test]
    fn strips_feature_credits() {
        assert_eq!(strip_features("Rapture (feat. Nadia Ali)"), "rapture");
        assert_eq!(
            strip_features("One More Time ft. Romanthony"),
            "one more time"
        );
        assert_eq!(strip_features("Plain Title"), "plain title");
    }

    #[test]
    fn splits_qualifiers_and_drops_neutral_ones() {
        assert_eq!(
            split_qualifier("Around the World (Radio Edit)"),
            ("around the world".into(), String::new())
        );
        assert_eq!(
            split_qualifier("Strobe (Deadmau5 Remix)"),
            ("strobe".into(), "deadmau5 remix".into())
        );
        assert_eq!(
            split_qualifier("Windowlicker [Remastered 2012]"),
            ("windowlicker".into(), String::new())
        );
    }

    #[test]
    fn exact_metadata_scores_top_marks() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        let m = pick_best("Daft Punk", "Around the World", &results).expect("match");
        assert_eq!(m.track_id, 1440857781);
        assert!(m.match_score > 0.98, "score {}", m.match_score);
        assert!(!m.uncertain);
        assert!(m.preview_url.is_some());
    }

    #[test]
    fn asking_for_a_specific_edit_gets_that_edit() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        let m = pick_best("Daft Punk", "Around the World (Radio Edit)", &results).unwrap();
        assert_eq!(m.track_id, 1440857999);
        assert!(!m.uncertain, "score {}", m.match_score);
    }

    #[test]
    fn a_neutral_qualifier_the_store_does_not_have_still_matches_the_plain_title() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        // Nothing in the fixture is called "(Remastered)", so the qualifier
        // must be ignored and the plainest title win.
        let m = pick_best("Daft Punk", "Around the World (Remastered)", &results).unwrap();
        assert_eq!(m.track_id, 1440857781);
        assert!(!m.uncertain, "score {}", m.match_score);
    }

    #[test]
    fn the_right_artist_wins_over_a_title_collision() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        // Red Hot Chili Peppers also have a track called "Around the World".
        let m = pick_best("Red Hot Chili Peppers", "Around the World", &results).unwrap();
        assert_eq!(m.track_id, 1500000001);
    }

    #[test]
    fn matching_is_deterministic() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        let a = pick_best("Daft Punk", "Around the World", &results).unwrap();
        let b = pick_best("Daft Punk", "Around the World", &results).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn the_original_wins_over_a_remix_of_the_same_title() {
        let results = parse_response(REMIX_FIXTURE).unwrap();
        let m = pick_best("deadmau5", "Strobe", &results).expect("match");
        assert_eq!(m.title, "Strobe", "picked {:?}", m.title);
    }

    #[test]
    fn asking_for_the_remix_gets_the_remix() {
        let results = parse_response(REMIX_FIXTURE).unwrap();
        let m = pick_best("deadmau5", "Strobe (Michael Woods Remix)", &results).unwrap();
        assert!(m.title.contains("Michael Woods"), "picked {:?}", m.title);
    }

    #[test]
    fn a_wrong_track_is_flagged_uncertain_rather_than_hidden() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        let m = pick_best("Some Other Act", "A Completely Different Song", &results).unwrap();
        assert!(m.uncertain, "score {}", m.match_score);
        // The metadata of whatever was picked comes back, so a bad match is
        // visible rather than something the caller has to infer.
        assert!(!m.artist.is_empty() && !m.title.is_empty());
    }

    #[test]
    fn candidates_without_a_preview_are_never_matched() {
        let results = parse_response(SEARCH_FIXTURE).unwrap();
        // "Around the World (Live)" in the fixture has no previewUrl.
        let m = pick_best("Daft Punk", "Around the World (Live)", &results).unwrap();
        assert!(m.preview_url.is_some());
        assert_ne!(m.title, "Around the World (Live)");
    }

    #[test]
    fn an_empty_result_set_yields_no_match() {
        let results = parse_response(EMPTY_FIXTURE).unwrap();
        assert!(pick_best("Nobody", "Nothing", &results).is_none());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse_response("not json at all").is_err());
        assert!(parse_response("{}").unwrap().is_empty());
    }

    #[test]
    fn urlencoding_covers_spaces_and_non_ascii() {
        assert_eq!(urlencode("daft punk"), "daft+punk");
        assert_eq!(urlencode("Röyksopp"), "R%C3%B6yksopp");
        assert_eq!(urlencode("a/b?c"), "a%2Fb%3Fc");
    }
}
