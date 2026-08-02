#include "curatorprompts.hpp"

#include <string>

namespace cml {
namespace {

// KEEP/DROP — does the row hold any content at all. Getting this wrong loses knowledge
// permanently, so both rubrics stay lenient here.

// Carried over verbatim from the Rust implementation: the curator's entire behaviour
// lives in this string, so edits to it are behaviour changes.
constexpr const char* kAssistantKeep = R"CML(You curate a developer's AI-assistant history into long-term memory. Only content with real, specific information — logical sense that stands on its own — belongs here. Everything else is noise and must be dropped.
KEEP (keep=true): a specific fact; a root cause; a decision and its reason; an explanation of how something works; a non-obvious gotcha or finding; a real measurement; or a recorded user preference/correction. Read the row alone — if you learn something concrete and reusable, keep it.
DROP (keep=false): generic phrases that carry no information ('what are we building?', 'here is where things stand', 'let me check', 'sounds good'); status and progress chatter ('doing X now', 'blocked on Y', 'walking it once more'); completion and acknowledgment reports ('Done', 'Fixed', 'Pushed', 'Set X to Y' — even with a value); pleasantries; and any row that would be meaningless out of its moment.
THE TEST: read the row by itself. Does it state specific, reusable information, or is it a generic / process / status phrase? Generic, process, or noise → DROP. When unsure, DROP.
)CML";

// The user rubric. Note it inverts the assistant rubric's tie-breaker: there, unsure
// means DROP; here, unsure about a correction means KEEP. A user row wrongly dropped
// costs a standing instruction, which is the most expensive loss in the whole index.
constexpr const char* kUserKeep = R"CML(You curate a developer's own messages to an AI assistant into long-term memory. Judge each message on whether a FUTURE session would act differently for having read it.
KEEP (keep=true):
- a correction, rejection, or complaint about the assistant's work — EVEN IF SHORT OR VAGUE ('wtf this doesnt touch real db?', 'there is no hyperlinks', 'still not working'). These are the highest-value rows; their meaning is relational, not standalone. When a message expresses dissatisfaction or contradicts something, ALWAYS keep.
- a stated preference, rule, or standing instruction ('never use X', 'always do Y by default').
- a standing instruction phrased CASUALLY — as philosophy, frustration, or an aside. These are the most-missed keeps. All of these are KEEP: 'well for now if it works - its good, lets just build it and change it later'; 'yes do all of these stop asking me'; 'delete it freely stop waiting'. A rule about HOW to work is a rule even when sworn, shrugged, or buried in a sentence about something else. Tone is not content.
- a decision, or a choice between options, with or without the reason.
- a concrete domain fact, value, setting, number, name, path, or requirement — INCLUDING facts the user states about their OWN system ('i created test mode, there are no waittimes between messages'). The user is the authority on their own build; such a statement is a spec, never chatter.
- a bug report carrying a symptom, or a specific feature request.
DROP (keep=false): pure steering that only moves the current turn along ('continue please im sorry', 'do your work now i opened it', 'yes please run it'); status and progress checks ('ok so it works?', 'everything is good now?', 'are we done?'); tooling/meta chatter about the chat itself ('how do i paste screenshot?', 'update all the plugins please'); bare reactions and pleasantries carrying no claim.
THE TEST: would a future session behave differently for having read this? If it only moved the current turn along, DROP. If it corrects, constrains, decides, or states a fact, KEEP. When unsure whether something is a correction or a standing instruction, KEEP. An undurable row is still a keep — refusing a gist NEVER makes it a drop.
)CML";

// DURABILITY — a second, independent axis deciding only whether a gist is minted. Shared
// by both rubrics because the bar does not depend on who spoke.
//
// Calibrated against the real corpus rather than guessed. A first draft said "a lasting
// fact about the user's environment" and minted 80% of rows — the model was obeying it
// correctly; the clause was wrong, because nearly every message is a true fact about his
// machine. Adding the five-minute test took it to 30%, and the decision and cost tests to
// 19% (~187 gists), which is the shipped bar.
constexpr const char* kDurability = R"CML(You decide ONE thing per row: does it deserve a gist — a permanent point on the developer's knowledge map? These rows have ALREADY been kept and stay searchable regardless. You are NOT deciding whether to keep anything.
A gist is expensive: it becomes a permanent point on the developer's knowledge map, which should hold only a few hundred hard-won facts across years of work. The overwhelming majority of kept rows get gist:"". Rows are never deleted for lacking a gist — they stay fully searchable — so refusing a gist costs nothing and is the correct default.
TEST 1, THE FIVE-MINUTE TEST: could a competent engineer find this in five minutes from documentation, a man page, `--help`, or one web search? Then gist:"". Rejects keyboard shortcuts, default settings, how-to steps, menu locations, flag meanings, version numbers, install commands, and explanations of how a well-known tool works. Being true, and being about the user's machine, is NOT enough.
TEST 2, THE DECISION TEST: would this fact change what someone DOES on a future, different task — or does it merely inform them? Only "changes what you do" survives.
TEST 3, THE COST TEST: was this expensive to learn — discovered by debugging, measuring, or being wrong first? A fact that was obvious once stated is not durable. Durable facts usually have the shape "X, not Y": they record the thing you would have got wrong.
DURABLE, and little else: a root cause where the obvious explanation was wrong; a gotcha that cost real time and no documentation warned about; a hard constraint of the user's own production system; a decision WITH its reason, or an option rejected and why; a measurement that CHANGED a conclusion; a standing rule or preference the user stated about how to work.
NOT DURABLE: completion reports and status; mode or config acknowledgments; plans; restatements of work performed; one-off requests; routine implementation details the code already documents; third-party behaviour that never bit this user in production; and any row that reports what happened rather than what was learned.
When unsure, gist:"". Two hundred hard-won facts are worth more than nine hundred true ones.
EVERY row in this batch already passed the keep filter. That tells you NOTHING about durability — most kept rows are not durable, and a batch where most rows get a gist is a batch you have judged wrong.
BUDGET: across a batch of 20 rows expect roughly 2 to 4 gists. Five or more means your bar has slipped; re-read the three tests and raise it.
For a durable row the gist is its reusable essence in at most 120 characters.

SEPARATELY, and for EVERY row whether or not it earns a gist, write `asks`: 3 to 5 short questions or phrasings the developer might type months from now when they want this row back. Two rules. Use THEIR words, not the row's — the entire point is to bridge vocabulary the row does not contain, so copying its sentences is worthless. And describe the SYMPTOM or the goal, not the solution, because that is what someone types before they know the answer. A row about "tap-drag lock lives in the xfconf pointers channel" should carry asks like "trackpad keeps selecting after I lift my finger", "how do I stop double-tap dragging on linux", "text highlights on its own when I move the cursor". For a row that is pure status or noise, asks:[].
Reply ONLY with JSON, keep always true, gist "" for every row that is not durable: {"verdicts":[{"id":<id>,"keep":true,"gist":"","asks":["...","..."]}]})CML";

// SCENE — L2, one summary per working session. Not a judgement: nothing is kept or
// dropped here, a whole session goes in and one paragraph comes out.
//
// The entire difficulty is the activity log. Asked to summarise a transcript, a model
// writes "worked on the parser, fixed a bug, ran the tests" — true, fluent, and worth
// nothing in six months, because it records that time passed rather than what is now
// known. So the prompt names that failure mode outright and gives `abandoned` as the
// honest exit for a session that established nothing.
//
// It is addressed to a BATCH, unlike its first draft. build_scenes sends four sessions
// per call (a single one would cost 76 calls instead of 19), and a prompt opening "you
// are given one working session" in front of four digests invites the model to merge
// them or answer only the first — a wrong reply shape on a paid call.
//
// The last two paragraphs are calibration, not decoration. Measured on the first 20 real
// sessions: 4 came back as pure activity logs, over the 3-in-20 bar the plan set, and
// EVERY summary — including the good ones — opened with a narration sentence and
// appended the finding after it ("Deployed platform v2 ... Key lessons: ..."). Two
// sessions that established nothing came back `solved`, because `abandoned` reads as
// "the work failed" unless it is said outright that it means "nothing was learned".
constexpr const char* kScene = R"CML(Each row you are given is one whole working session between a developer and an assistant. For EVERY row, produce a title, a summary of two to three sentences, and an outcome of exactly `solved`, `abandoned`, or `ongoing`, carrying that row's id through unchanged. Reply with exactly as many scenes as there were rows — never merge two sessions into one. A row may be elided in the middle; the opening and the ending are always present, and the ending is what decides the outcome.
The summary must state what was actually learned or decided and why — the mechanism, the root cause, the rule that came out of it. It must NOT be an activity log. "worked on the parser", "debugged the hook", "made several changes" are failures: they describe that time passed, not what is now known.
Do NOT open with what was done. "Deployed X", "Updated three plugins", "The build completed" — narrating first and appending the insight afterwards wastes the sentence that actually gets read. Open with the finding itself: the cause, the constraint, the decision and its reason, the measurement that changed a conclusion. Name what someone would otherwise get wrong.
A session can succeed at its task and still teach nothing. Deploying a service, bumping versions, applying a known fix: if nothing is now known that a competent engineer would not have assumed, the outcome is `abandoned` however well the work went — one honest sentence saying so, and no significance invented for it. `solved` means something is now known that was not known before. `ongoing` means the finding is real but the work is unfinished.
Reply with a JSON object: {"scenes": [{"id": <id>, "title": ..., "summary": ..., "outcome": ...}]}. No prose outside it.)CML";

constexpr const char* kKeepReply =
    "Reply ONLY with JSON: {\"verdicts\":[{\"id\":<id>,\"keep\":true|false,\"gist\":\"\"}]}"
    " — always leave gist empty; durability is judged in a separate pass.";

const std::string& assistant_prompt() {
    static const std::string p = std::string(kAssistantKeep) + kKeepReply;
    return p;
}

const std::string& user_prompt() {
    static const std::string p = std::string(kUserKeep) + kKeepReply;
    return p;
}

}  // namespace

std::string_view curator_prompt(Rubric r) {
    // A switch with no default, deliberately. This was an if plus a ternary, which meant
    // a new rubric with no prompt of its own silently got the ASSISTANT one — a wrong
    // system prompt on a paid call, invisible to -Werror. -Wswitch sees it for free.
    switch (r) {
        case Rubric::Assistant: return assistant_prompt();
        case Rubric::User: return user_prompt();
        case Rubric::Durability: return kDurability;
        case Rubric::Scene: return kScene;
    }
    return assistant_prompt();  // unreachable; a scoped enum can still hold a stray value
}

Rubric rubric_for_role(std::string_view role) {
    return role == "user" ? Rubric::User : Rubric::Assistant;
}

}  // namespace cml
