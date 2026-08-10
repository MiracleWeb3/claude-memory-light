//! The curator's rubrics. One per row author, because the two are not judged alike.
//!
//! An assistant row earns its place by standing alone: read it by itself and you
//! either learn something reusable or you don't. A *user* row often cannot pass that
//! test and still matters enormously — "wtf this doesnt touch real db?" reads as
//! noise in isolation and is the origin of a standing rule. Its value is relational.
//! Measured on this corpus (Jul 26 2026): judging user rows with the assistant rubric
//! dropped 3 messages that had already become permanent rules.
//!
//! The prompts themselves are prose, so they live in `prompts/*.txt` rather than in
//! quoted string literals — editable and diffable as the English they are. They are
//! still `include_str!`, not read at runtime: the curator's whole behaviour is in
//! these strings, and a missing file must never become a wrong system prompt on a
//! paid call. Editing one of these files IS a behaviour change.

/// Assistant and User judge KEEP (is there content?). Durability is a SEPARATE second
/// call judging only whether a kept row earns a gist.
///
/// It has to be its own call. Appending the durability clause to a keep rubric was
/// measured at 45-84% minted against 16% for the same clause asked on its own: the
/// keep list is deliberately generous, and that generosity bleeds into the gist
/// decision when one call is asked to do both.
///
/// Scene is the odd one out: it judges nothing and drops nothing. It reads a whole
/// session and writes one L2 summary of it, so it is a rubric only in the sense that
/// it is a system prompt selected here. It rides the same wire as the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rubric {
    Assistant,
    User,
    Durability,
    Scene,
}

impl Rubric {
    /// The system prompt for this rubric.
    ///
    /// An exhaustive match, deliberately. In C++ this was an `if` plus a ternary, so a
    /// new rubric with no prompt of its own silently got the ASSISTANT one — a wrong
    /// system prompt on a paid call, invisible to the compiler. Here a new variant
    /// stops the build until it has a prompt.
    pub const fn prompt(self) -> &'static str {
        match self {
            Rubric::Assistant => include_str!("prompts/assistant.txt"),
            Rubric::User => include_str!("prompts/user.txt"),
            Rubric::Durability => include_str!("prompts/durability.txt"),
            Rubric::Scene => include_str!("prompts/scene.txt"),
        }
    }

    /// Which rows this rubric owns, as a SQL predicate against `mem`.
    ///
    /// User rows were excluded until Jul 26 2026, which left every message the
    /// developer typed judged by nothing but a 34-word ack list and a four-word floor
    /// — "continue please" was caught, "continue please im sorry" was not.
    ///
    /// Durability and Scene select no rows of their own (they are handed rows that
    /// another pass already chose), but they are spelled out rather than defaulted:
    /// see `prompt` for what a silent fallthrough costs here.
    pub const fn row_filter(self) -> &'static str {
        match self {
            Rubric::User => "role = 'user'",
            Rubric::Assistant | Rubric::Durability | Rubric::Scene => {
                "role IN ('assistant','summary')"
            }
        }
    }
}

/// Which rubric judges a row of this role. Unknown roles fall back to Assistant.
pub fn rubric_for_role(role: &str) -> Rubric {
    if role == "user" {
        Rubric::User
    } else {
        Rubric::Assistant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user rubric exists to stop one specific loss: a standing instruction
    /// phrased casually, dropped for reading like chatter. These assertions fail if
    /// that guard is ever edited back out of the prompt.
    #[test]
    fn user_rows_are_judged_by_their_own_rubric() {
        assert_eq!(rubric_for_role("user"), Rubric::User);
        assert_eq!(rubric_for_role("assistant"), Rubric::Assistant);
        assert_eq!(rubric_for_role("summary"), Rubric::Assistant, "summaries are not user rows");

        let u = Rubric::User.prompt();
        let a = Rubric::Assistant.prompt();
        assert_ne!(u, a, "the two rubrics are different prompts");
        assert!(u.contains("ALWAYS keep"), "corrections are an unconditional keep");
        assert!(u.contains("Tone is not content"), "a sworn rule is still a rule");
        assert!(u.contains("their own build"), "the user is authority on their system");
        // The assistant rubric breaks ties toward DROP; the user rubric must not.
        assert!(a.contains("When unsure, DROP"), "assistant tie-break unchanged");
        assert!(!u.contains("When unsure, DROP"), "user rubric never defaults to drop");
    }

    /// Keeping and gisting are independent axes. The three calibrated tests live in
    /// the durability prompt and nowhere else — each one measurably lowers the mint
    /// rate: the clause alone 80%, +five-minute 30%, +decision+cost 19%.
    #[test]
    fn durability_is_judged_apart_from_keeping() {
        let dur = Rubric::Durability.prompt();
        for test in ["FIVE-MINUTE TEST", "DECISION TEST", "COST TEST"] {
            assert!(dur.contains(test), "{test} survives");
        }
        assert!(dur.contains(r#"When unsure, gist:"""#), "refusing a gist is the default");
        assert!(dur.contains("NOT deciding whether to keep"), "the pass cannot delete");

        // ...and it must NOT leak into the keep rubrics.
        for keep in [Rubric::Assistant.prompt(), Rubric::User.prompt()] {
            assert!(!keep.contains("FIVE-MINUTE TEST"), "keep rubric stays free of the gist bar");
            assert!(keep.contains("durability is judged in a separate pass"));
        }
        // The asymmetry that makes a strict bar safe: an empty gist never means drop.
        assert!(Rubric::User.prompt().contains("NEVER makes it a drop"));
        for r in [Rubric::Assistant, Rubric::User, Rubric::Durability] {
            assert!(r.prompt().contains("Reply ONLY with JSON"), "{r:?} states its contract");
        }
    }

    /// The scene rubric earns its place only by refusing the activity log — the
    /// summary a model writes by default, fluent and worth nothing months later.
    #[test]
    fn scene_rubric_forbids_activity_logs() {
        let p = Rubric::Scene.prompt();
        assert!(p.contains("worked on"), "the prompt names the failure mode it refuses");
        assert_ne!(p, Rubric::Assistant.prompt(), "Scene reaches its own prompt");
        for word in ["solved", "abandoned", "ongoing"] {
            assert!(p.contains(word), "the outcome vocabulary is closed and stated");
        }
        assert!(p.contains(r#""scenes""#), "and the object shape is spelled out");
    }

    #[test]
    fn only_user_rows_go_to_the_user_rubric() {
        assert_eq!(Rubric::User.row_filter(), "role = 'user'");
        assert!(Rubric::Assistant.row_filter().contains("summary"));
    }
}
