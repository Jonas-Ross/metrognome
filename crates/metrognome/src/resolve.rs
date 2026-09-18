//! Resolving an artist and title to an iTunes store track with a preview URL.
//!
//! selecta has Music.app persistent IDs, not store IDs, so artist/title search
//! is the primary path and matching is necessarily fuzzy. Matching is split
//! from HTTP — [`pick_best`] is pure, driven in tests from recorded fixtures —
//! and every response carries a match score, so a wrong match is visible.

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::ratelimit::RateLimiter;
use crate::types::TrackMatch;

/// Refuse a search response larger than this.
///
/// A `limit=25` response is tens of KB. The ceiling exists because the body is
/// buffered whole, and `--api-base-url` and a redirect both point this at
/// servers Apple does not run.
const MAX_SEARCH_BYTES: usize = 1024 * 1024;

/// Bumped whenever a change to matching could make a query resolve to a
/// different track.
///
/// The resolution cache is keyed by query text, so without this a matching fix
/// would never reach anyone whose cache is already warm.
pub const MATCHER_VERSION: u32 = 2;

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
/// Stripping these matches "(Radio Edit)" to the plain title without also
/// matching "(Deep Dish Remix)", which is a different recording with its own
/// tempo and key.
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

/// Tokens that introduce a featured-artist credit.
const FEATURE_MARKERS: [&str; 4] = ["feat", "featuring", "ft", "with"];

/// Remove a trailing "feat. …" / "featuring …" / "ft. …" clause.
///
/// The most common cosmetic difference between a library's metadata and the
/// store's, and never a different recording.
///
/// `whole_may_be_credit` allows the string to reduce to nothing. A
/// parenthesized qualifier often is pure credit — "Song (feat. Guest)" — but a
/// title opening with "With" is more likely real, so its first token is kept.
fn strip_features(s: &str, whole_may_be_credit: bool) -> String {
    let n = normalize(s);
    let tokens: Vec<&str> = n.split_whitespace().collect();
    let keep = usize::from(!whole_may_be_credit);
    match tokens
        .iter()
        .skip(keep)
        .position(|t| FEATURE_MARKERS.contains(t))
    {
        Some(i) => tokens[..keep + i].join(" "),
        None => n,
    }
}

/// Split a title into its core and its parenthesized qualifier.
fn split_qualifier(title: &str) -> (String, String) {
    let mut core = String::new();
    let mut qual = String::new();
    let mut depth = 0i32;
    for ch in title.chars() {
        match ch {
            // The bracket itself becomes a space rather than vanishing: two
            // adjacent groups, "(Club Mix) [feat. Guest]", would otherwise fuse
            // into "club mixfeat guest" and neither part would be recognized.
            '(' | '[' => {
                depth += 1;
                qual.push(' ');
            }
            ')' | ']' => {
                depth = (depth - 1).max(0);
                qual.push(' ');
            }
            _ if depth > 0 => qual.push(ch),
            _ => core.push(ch),
        }
    }
    // A qualifier that is only a credit ("(feat. Guest)") must reduce to
    // nothing, or a store title carrying one scores far below a query without
    // it even though they are the same recording.
    let mut qual = strip_features(&qual, true);
    for neutral in NEUTRAL_QUALIFIERS {
        qual = qual.replace(neutral, " ");
    }
    // A bare year is a reissue marker, not a different recording.
    qual = qual
        .split_whitespace()
        .filter(|t| !(t.len() == 4 && t.chars().all(|c| c.is_ascii_digit())))
        .collect::<Vec<_>>()
        .join(" ");
    (strip_features(&core, false), qual.trim().to_string())
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
/// A neutral qualifier is stripped from both sides, so without this the plain
/// title and "(Radio Edit)" tie. Small on purpose: it settles a tie rather than
/// outranking a better match.
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

    let q_artist = strip_features(query_artist, false);
    let c_artist = strip_features(cand.artist_name.as_deref().unwrap_or(""), false);
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
/// A track with no `previewUrl` cannot be analyzed, however well its metadata
/// scores.
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

/// Bytes of a title that sit inside brackets, before any neutral-qualifier
/// stripping. Used only as a tiebreak: of two equally good matches, the one
/// carrying less unrequested text is the one the caller meant.
fn raw_qualifier_len(c: &ItunesTrack) -> usize {
    let title = c.track_name.as_deref().unwrap_or("");
    let mut depth = 0i32;
    let mut outside = 0usize;
    for ch in title.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = (depth - 1).max(0),
            _ if depth == 0 => outside += ch.len_utf8(),
            _ => {}
        }
    }
    title.len().saturating_sub(outside)
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
        let body = crate::fetch::read_bounded(resp, MAX_SEARCH_BYTES, "search response").await?;
        String::from_utf8(body).map_err(|e| Error::Http(format!("read body {url}: {e}")))
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
            // Not NoPreview: that says the track exists and can never be
            // analyzed, which a consumer may record and never retry. A missing
            // row means the ID is wrong or regional.
            .ok_or(Error::NotFound { track_id })
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
        assert_eq!(
            strip_features("Rapture (feat. Nadia Ali)", false),
            "rapture"
        );
        assert_eq!(
            strip_features("One More Time ft. Romanthony", false),
            "one more time"
        );
        assert_eq!(strip_features("Plain Title", false), "plain title");
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
    fn the_plainest_title_is_the_tiebreak() {
        let plain = ItunesTrack {
            track_id: Some(1),
            track_name: Some("Strobe".into()),
            artist_name: Some("deadmau5".into()),
            collection_name: None,
            release_date: None,
            primary_genre_name: None,
            preview_url: Some("https://example/p.m4a".into()),
            track_time_millis: None,
        };
        let reissue = ItunesTrack {
            track_id: Some(2),
            track_name: Some("Strobe (Remastered 2019)".into()),
            ..plain.clone()
        };
        assert!(raw_qualifier_len(&reissue) > raw_qualifier_len(&plain));
        // Both score identically once the neutral qualifier is stripped, so
        // the tiebreak is what decides.
        let picked = pick_best("deadmau5", "Strobe", &[reissue, plain]).unwrap();
        assert_eq!(picked.track_id, 1);
    }

    #[test]
    fn a_store_side_feature_credit_does_not_make_the_match_uncertain() {
        // The store spells the credit in the title; the library does not. That
        // is cosmetic, so the qualifier must reduce to nothing rather than
        // scoring as a different edit.
        assert_eq!(
            split_qualifier("Rapture (feat. Nadia Ali)"),
            ("rapture".into(), String::new())
        );
        let credited = ItunesTrack {
            track_id: Some(1),
            track_name: Some("Rapture (feat. Nadia Ali)".into()),
            artist_name: Some("iiO".into()),
            collection_name: None,
            release_date: None,
            primary_genre_name: None,
            preview_url: Some("https://example/p.m4a".into()),
            track_time_millis: None,
        };
        let m = pick_best("iiO", "Rapture", &[credited]).expect("match");
        assert!(
            !m.uncertain,
            "a feature credit alone must not flag a match uncertain, score {}",
            m.match_score
        );

        // A credit inside a real qualifier is stripped without taking the rest
        // of the qualifier with it.
        assert_eq!(
            split_qualifier("Strobe (Club Mix) [feat. Guest]"),
            ("strobe".into(), "club mix".into())
        );
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
