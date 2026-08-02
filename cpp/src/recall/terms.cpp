// What a prompt is asking about, and how much of it a row shares. Both are the floor the
// retrieval loop gates on, and both are exposed for the selftest — a gate nobody can call
// directly is a gate nobody can check.

#include "recall.hpp"

#include <algorithm>
#include <cctype>
#include <iterator>
#include <string>
#include <unordered_set>

#include "recall/internal.hpp"

namespace cml {
namespace {

constexpr std::size_t kMaxTerms = 12;    // an OR query over a whole paste is not a query

// Function words carry no retrieval signal but would dominate an OR query.
constexpr std::string_view kStop[] = {
    "the",   "and",   "for",    "are",   "but",   "not",    "you",  "all",   "can",
    "was",   "one",   "our",    "out",   "how",   "its",    "who",  "did",   "yes",
    "get",   "has",   "him",    "his",   "she",   "her",    "too",  "use",   "why",
    "what",  "this",  "that",   "with",  "have",  "from",   "they", "been",  "were",
    "when",  "your",  "said",   "each",  "which", "them",   "then", "than",  "some",
    "would", "there", "their",  "about", "into",  "just",   "like", "make",  "made",
    "does",  "doing", "dont",   "cant",  "wont",  "should", "could", "need", "want",
    "please","okay",  "also",   "only",  "very",  "more",   "most", "much",  "still",
    "again", "now",   "here",   "let",   "lets",  "will",   "any",  "because",
};

bool is_stop(const std::string& w) {
    return std::find(std::begin(kStop), std::end(kStop), std::string_view(w)) != std::end(kStop);
}

}  // namespace

std::vector<std::string> content_terms(std::string_view prompt) {
    std::vector<std::string> out;
    std::unordered_set<std::string> seen;
    std::string cur;
    const auto flush = [&] {
        if (cur.size() >= 3 && !is_stop(cur) && seen.insert(cur).second) out.push_back(cur);
        cur.clear();
    };
    for (const char c : prompt) {
        const auto u = static_cast<unsigned char>(c);
        // Letters, digits and any non-ASCII byte stay in the token, so a Russian word
        // survives as one unit instead of shattering into per-byte noise.
        if (std::isalnum(u) || u >= 0x80)
            cur.push_back(static_cast<char>(std::tolower(u)));
        else
            flush();
    }
    flush();
    if (out.size() > kMaxTerms) out.resize(kMaxTerms);
    return out;
}

std::size_t term_overlap(std::string_view text, const std::vector<std::string>& terms) {
    const std::string hay = lower(text);
    std::size_t n = 0;
    for (const auto& t : terms)
        if (hay.find(t) != std::string::npos) ++n;
    return n;
}

}  // namespace cml
