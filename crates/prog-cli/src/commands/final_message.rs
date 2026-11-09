//! Deterministic reading of an agent's final message (#305, #314): does it
//! claim that tests pass or that a failure is fixed, and does it tell the user
//! that verification is still open? Plain phrase rules, no model.
//!
//! Both rules are tuned against false alarms found in a replay of 1,306 real
//! turn ends (#305): pass counts that prog could not prove, CI claims with no
//! local evidence, benchmark tallies, deliberate red-on-base runs, and
//! disclosures in unusual wording. When in doubt, a sentence is neither a
//! claim nor a disclosure.

use std::sync::OnceLock;

use regex::Regex;

/// What a success claim asserts. Only `Fixed` is contradicted by `NOT
/// PROVEN`: "54 passed" is true of a run that could not prove an earlier
/// failure fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimKind {
    /// "fixed", "proven fixed", "now passes".
    Fixed,
    /// "all tests pass", "the suite is green", "N passed, 0 failed".
    Passing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Claim {
    pub(crate) kind: ClaimKind,
    /// The claiming sentence, cut to a short quote.
    pub(crate) quote: String,
}

const MAX_QUOTE_CHARS: usize = 120;

/// Phrases with which a final message tells the user that verification is
/// still open. Matched case-insensitively as plain substrings. They are kept
/// narrow so that a claim of success cannot match one: no bare "tests are
/// failing" (it also matches "no tests are failing") and no "tests fail" (it
/// also matches "the tests failed before the fix").
const OPEN_VERIFICATION_PHRASES: &[&str] = &[
    "still fail",
    "still broken",
    "still red",
    "not fixed",
    "isn't fixed",
    "is not fixed",
    "remain failing",
    "remains failing",
    "failures remain",
    "not proven",
    "unproven",
    "could not verify",
    "couldn't verify",
    "cannot verify",
    "can't verify",
    "not verified",
    "unverified",
    "did not run",
    "didn't run",
    "have not run",
    "haven't run",
    "has not been run",
    "have not been run",
    "were not run",
    "was not run",
    "wasn't run",
    "weren't run",
    "not rerun",
    "did not rerun",
    "didn't rerun",
    "without running",
    "not run the tests",
    "not run any tests",
    // A deliberate red-on-base run: the newest run fails on purpose.
    "fails on the original",
    "fails without the fix",
    "fails before the fix",
    "fails as expected",
    "failed as expected",
    "expected to fail",
    "red on base",
    "red-on-base",
];

/// Whether the message tells the user that tests are failing, unproven, or
/// were not run, or that the newest failing run is a deliberate reproduction.
pub(crate) fn discloses_open_verification(message: &str) -> bool {
    let message = normalize(message)
        // "did not run into problems" is not "did not run".
        .replace(" run into", " ran-into");
    OPEN_VERIFICATION_PHRASES
        .iter()
        .any(|phrase| message.contains(phrase))
}

/// The first sentence that claims tests pass or a failure is fixed, ignoring
/// code, quotes, hedged or conditional sentences, CI claims, and tallies of
/// anything other than local tests.
pub(crate) fn success_claim(message: &str) -> Option<Claim> {
    let body = rules().code_or_quote.replace_all(message, " ");
    for sentence in rules().sentence.split(&body) {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        let lower = normalize(sentence);
        if rules().hedge.is_match(&lower)
            || rules().not_local_tests.is_match(&lower)
            || discloses_open_verification(&lower)
        {
            continue;
        }
        let kind = if rules().fixed.is_match(&lower) {
            Some(ClaimKind::Fixed)
        } else if rules().passing.is_match(&lower) || passing_tally(&lower) {
            Some(ClaimKind::Passing)
        } else {
            None
        };
        if let Some(kind) = kind {
            return Some(Claim {
                kind,
                quote: quote(sentence),
            });
        }
    }
    None
}

/// "309 passed", "54/54 passed", "12 passed, 0 failed", "0 failed": a tally
/// with nothing failing. "28/34 passed" or "3 passed, 1 failed" is not a
/// success claim.
fn passing_tally(sentence: &str) -> bool {
    if let Some(captures) = rules().fraction.captures(sentence) {
        return captures[1] == captures[2];
    }
    if rules().failed_nonzero.is_match(sentence) {
        return false;
    }
    rules().passed_count.is_match(sentence) || rules().zero_failed.is_match(sentence)
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .replace(['\u{2019}', '\u{2018}'], "'")
        .replace(['\u{201c}', '\u{201d}'], "\"")
}

fn quote(sentence: &str) -> String {
    let flat = sentence.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = flat
        .trim_end_matches(['.', '!', ',', ':', ';'])
        .trim_matches('*')
        .to_string();
    if flat.chars().count() <= MAX_QUOTE_CHARS {
        flat
    } else {
        let cut = flat.chars().take(MAX_QUOTE_CHARS - 1).collect::<String>();
        format!("{cut}…")
    }
}

struct Rules {
    code_or_quote: Regex,
    sentence: Regex,
    hedge: Regex,
    not_local_tests: Regex,
    fixed: Regex,
    passing: Regex,
    fraction: Regex,
    failed_nonzero: Regex,
    passed_count: Regex,
    zero_failed: Regex,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let regex = |pattern: &str| Regex::new(pattern).expect("final-message rule compiles");
        Rules {
            // Fenced and inline code, quoted text, and quoted lines.
            code_or_quote: regex(
                r#"(?s:```.*?```)|`[^`\n]*`|"[^"\n]*"|“[^”\n]*”|(?m:^[ \t]*>[^\n]*$)"#,
            ),
            sentence: regex(r"(?:[.!?;]\s+|\n+|\s+-\s+|\|)"),
            hedge: regex(
                r"\b(if|once|would|should|will|until|whether|expect|expected|unless|not|never|no longer|could|can|cannot|can't|unable|might|may|hopefully|probably|pending|need to|needs to|to make|before you|aim|goal|plan|want)\b|n't\b|'ll\b|'d\b|\?",
            ),
            not_local_tests: regex(
                r"\b(ci|checks?|workflows?|jobs?|github|actions|pipeline|benchmarks?|evals?|intents?|timeouts?|tasks?|trials?|cases? solved|deploy\w*|review(ed|s)?)\b",
            ),
            fixed: regex(
                r"\b(proven|verified) fixed\b|\bfix(es)? (is|are) proven\b|\b(is|are|now) fixed\b|\bnow pass(es)?\b",
            ),
            passing: regex(
                r"\ball (of )?(the )?(\d[\d,]* )?(unit |integration |e2e |focused |targeted |regression )?tests? (now )?pass(ed|es|ing)?\b|\b(test )?suites? (is |are )?(now )?(all )?(passing|green)\b|\ball green\b|\btests? (are |is )?(now )?(all )?(passing|green)\b",
            ),
            fraction: regex(r"\b(\d[\d,]*)\s*/\s*(\d[\d,]*) (tests? )?pass(ed|ing)?\b"),
            failed_nonzero: regex(r"\b[1-9]\d* (tests? )?(failed|failing|errors?)\b"),
            passed_count: regex(r"\b\d[\d,]* (\w+ ){0,2}tests? pass(ed|ing)?\b|\b\d[\d,]* passed\b"),
            zero_failed: regex(r"\b0 (tests? )?failed\b"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(message: &str) -> Option<ClaimKind> {
        success_claim(message).map(|claim| claim.kind)
    }

    #[test]
    fn plain_success_claims_are_recognized() {
        for (message, kind) in [
            ("All tests pass.", ClaimKind::Passing),
            ("All 42 tests passed.", ClaimKind::Passing),
            (
                "Validation: 1,432 tests pass; lint and build pass.",
                ClaimKind::Passing,
            ),
            ("- Unit/API tests: 54/54 passed", ClaimKind::Passing),
            ("The suite is green.", ClaimKind::Passing),
            ("**34 tests passed**.", ClaimKind::Passing),
            ("Local: 233 passed, 0 failed.", ClaimKind::Passing),
            (
                "The earlier failures are now proven fixed.",
                ClaimKind::Fixed,
            ),
            ("t::adds now passes.", ClaimKind::Fixed),
            (
                "The bug is fixed and the regression test passes.",
                ClaimKind::Fixed,
            ),
        ] {
            assert_eq!(claim(message), Some(kind), "{message}");
        }
    }

    /// The false-alarm classes from the #305 replay, plus hedges.
    #[test]
    fn non_claims_are_not_claims() {
        for message in [
            // CI claims have no local evidence.
            "All 8 CI checks pass.",
            "PR #293 is open and all 8 CI checks pass.",
            "Both are ready for review, with all CI checks passing.",
            // Tallies of something other than local tests, or with failures.
            "The 100-intent provider benchmark is still running: 28/34 passed, with six timeouts.",
            "28/34 passed.",
            "3 passed, 1 failed.",
            "Lint/build pass; full suite hit reported provider fixture failures.",
            // Hedged, conditional, or planned.
            "Once the tests pass, I'll open the PR.",
            "Say go and I'll do the rewrite as a PR with the docs tests green.",
            "Buy later, each only after a free-data test passes.",
            "Tests should pass now.",
            "Do all tests pass?",
            "I could not run the tests.",
            // Quoted or code.
            "The issue title says \"all tests pass\".",
            "Run `cargo test` until it says `0 failed`.",
            "> all tests pass",
            // Disclosures.
            "Tests are still failing (1 failed: t::adds).",
            "The fix is not proven yet.",
        ] {
            assert_eq!(success_claim(message), None, "{message}");
        }
    }

    #[test]
    fn disclosures_are_recognized_and_claims_never_count_as_disclosures() {
        for honest in [
            "The tests are still failing: add() returns a - b.",
            "Tests are still failing (1 failed: t::adds); not fixed per your request.",
            "I didn't run the tests after the last edit.",
            "prog says the fix is NOT PROVEN, because a test was skipped.",
            "The new regression test fails on the original code, as it should.",
            "It fails as expected before the fix.",
        ] {
            assert!(discloses_open_verification(honest), "{honest}");
        }
        for claim in [
            "All tests pass.",
            "The tests failed before the fix and now pass.",
            "No tests are failing.",
            "I did not run into any problems; everything is green.",
            "Fixed: 12 passed, 0 failed.",
        ] {
            assert!(!discloses_open_verification(claim), "{claim}");
        }
    }

    #[test]
    fn quotes_are_short_and_single_line() {
        let long = format!("All tests pass {}.", "and more ".repeat(40));
        let claim = success_claim(&long).unwrap();
        assert!(claim.quote.chars().count() <= MAX_QUOTE_CHARS);
        assert!(!claim.quote.contains('\n'));
    }
}
