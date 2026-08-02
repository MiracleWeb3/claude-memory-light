// Session transcripts -> searchable rows.
//
// The half of indexing with real judgement in it: which entries are worth a row at all.
// Notes are whole-file and trivial by comparison, so they live next door in notes.cpp.
#include <cctype>
#include <string>
#include <string_view>
#include <unordered_set>

#include "budget.hpp"
#include "curate.hpp"
#include "files.hpp"
#include "noise.hpp"
#include "paths.hpp"
#include "transcript.hpp"
#include "utf8.hpp"

namespace cml::idx {
namespace {

// Signal floor. Short assistant rows are mode-acks. Short user rows ("please",
// "ok done", "522556") are never how anyone finds anything in a search index, and
// the assistant reply that follows carries the answer and is kept anyway.
// ponytail: word count, not semantics — drop to 2 if real messages start vanishing.
constexpr std::size_t kUserMinWords = 4;

std::unordered_set<std::string> existing_keys(Db& db) {
    // Continued sessions leave overlapping rows across two transcript files, so dedup
    // against everything already indexed (this file's own rows have just been deleted).
    std::unordered_set<std::string> keys;
    Stmt s(db, "SELECT session, ts, role, substr(text,1,64) FROM mem");
    while (s.step()) keys.insert(stable_key(s.text(0), s.text(1), s.text(2), s.text(3)));
    return keys;
}

// Rust counts the signal floor on text.trim(); counting untrimmed would let a
// whitespace-padded stub slip past a floor the Rust binary enforces.
std::string_view trimmed(std::string_view s) {
    std::size_t b = 0, e = s.size();
    while (b < e && std::isspace(static_cast<unsigned char>(s[b]))) ++b;
    while (e > b && std::isspace(static_cast<unsigned char>(s[e - 1]))) --e;
    return s.substr(b, e - b);
}

std::size_t word_count(std::string_view s) {
    std::size_t n = 0;
    bool in_word = false;
    for (const char c : s) {
        const bool space = std::isspace(static_cast<unsigned char>(c));
        if (!space && !in_word) ++n;
        in_word = !space;
    }
    return n;
}

}  // namespace

Counts index_transcripts(Db& db, bool force, const Budget& budget) {
    Counts total;
    const auto blocked = forgotten_set(db);
    const fs::path projects = home() / ".claude/projects";
    std::error_code ec;
    if (!fs::is_directory(projects, ec)) return total;

    for (const auto& pdir : fs::directory_iterator(projects, ec)) {
        if (!pdir.is_directory()) continue;
        const std::string project = project_label(pdir.path().filename().string());

        // ponytail: top-level session files only; subagent transcripts are skipped.
        std::error_code inner;
        for (const auto& f : fs::directory_iterator(pdir.path(), inner)) {
            const fs::path path = f.path();
            if (budget.spent()) return total;  // resumable: files commit individually
            if (path.extension() != ".jsonl") continue;
            FileMeta fm;
            if (!meta_of(path, fm)) continue;
            const std::string pstr = path.string();
            if (!force && unchanged(db, pstr, fm)) continue;

            const std::string session_fallback = path.stem().string();
            db.exec("BEGIN");
            drop_rows_for_file(db, pstr);
            auto seen = existing_keys(db);

            const auto entries = parse_transcript(pstr, session_fallback);
            Stmt ins(db,
                     "INSERT INTO mem(text, role, project, session, ts, file) "
                     "VALUES(?1,?2,?3,?4,?5,?6)");
            Stmt wins(db,
                      "INSERT INTO work(text, role, project, session, ts, file) "
                      "VALUES(?1,?2,?3,?4,?5,?6)");
            std::size_t n = 0;
            for (std::size_t i = 0; i < entries.size(); ++i) {
                const Entry& e = entries[i];
                const char* role = nullptr;
                switch (e.kind) {
                    case Entry::Kind::Summary: role = "summary"; break;
                    case Entry::Kind::UserHuman: role = "user"; break;
                    case Entry::Kind::AssistantText:
                        if (!turn_final(entries, i)) continue;
                        role = "assistant";
                        break;
                    // The work itself: commands and their output. Short results are
                    // acknowledgements ("OK", "1 file changed") and carry no search
                    // surface, so they are dropped the way empty text is.
                    case Entry::Kind::AssistantTool:
                    case Entry::Kind::UserTool:
                        if (e.text.size() < 40) continue;
                        wins.reset();
                        wins.bind(1, e.text).bind(2, "tool").bind(3, project)
                            .bind(4, e.session).bind(5, e.ts).bind(6, pstr);
                        if (wins.run()) ++total.rows;
                        continue;
                    default: continue;
                }
                const std::string& text = e.text;
                if (is_noise(text)) continue;

                const std::size_t len = utf8_len(trimmed(text));
                const bool assistant = (e.kind == Entry::Kind::AssistantText);
                if ((assistant && len < 80) || len < 4) continue;
                if (e.kind == Entry::Kind::UserHuman && word_count(text) < kUserMinWords) continue;

                const std::string ts = (e.kind == Entry::Kind::Summary) ? "" : e.ts;
                const std::string sid =
                    (e.kind == Entry::Kind::Summary) ? session_fallback : e.session;
                const std::string key = stable_key(sid, ts, role, text);
                if (blocked.count(key)) continue;
                if (!seen.insert(key).second) continue;

                ins.reset();
                ins.bind(1, text).bind(2, role).bind(3, project).bind(4, sid).bind(5, ts).bind(6, pstr);
                if (ins.run()) ++n;
            }
            upsert_file(db, pstr, fm);
            db.exec("COMMIT");
            ++total.files;
            total.rows += n;
        }
    }
    return total;
}

}  // namespace cml::idx
