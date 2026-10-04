use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

pub struct FuzzyMatch {
    pub index: usize,
    pub score: u32,
}

/// Best `limit` candidates for `query`, highest score first. Ties keep the
/// candidates' original order. `paths` tunes scoring for file paths
/// (separators are word boundaries, the file name counts more).
pub fn fuzzy_match<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
    query: &str,
    limit: usize,
    paths: bool,
) -> Vec<FuzzyMatch> {
    let config = if paths {
        Config::DEFAULT.match_paths()
    } else {
        Config::DEFAULT
    };
    let mut matcher = Matcher::new(config);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut matches: Vec<FuzzyMatch> = Vec::new();
    for (index, text) in candidates.into_iter().enumerate() {
        let haystack = Utf32Str::new(text, &mut buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            matches.push(FuzzyMatch { index, score });
        }
    }
    matches.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    matches.truncate(limit);
    matches
}

/// Fills in match positions for the few candidates that end up on screen.
pub fn positions(text: &str, query: &str, paths: bool) -> Vec<u32> {
    let config = if paths {
        Config::DEFAULT.match_paths()
    } else {
        Config::DEFAULT
    };
    let mut matcher = Matcher::new(config);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut indices = Vec::new();
    pattern.indices(Utf32Str::new(text, &mut buf), &mut matcher, &mut indices);
    indices.sort_unstable();
    indices.dedup();
    indices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_file_name_matches() {
        let files = [
            "src/editor/mod.rs",
            "src/components/editor-preview.tsx",
            "README.md",
        ];
        let m = fuzzy_match(files, "edprev", 10, true);
        assert_eq!(m[0].index, 1);
        assert!(fuzzy_match(files, "zzz", 10, true).is_empty());
        assert_eq!(positions("README.md", "rdm", true), vec![0, 3, 4]);
    }
}
