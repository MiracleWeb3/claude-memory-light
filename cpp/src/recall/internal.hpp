// The one helper the recall translation units share: the overlap floor in terms.cpp and
// the echo check in retrieve.cpp must fold case the same way or they stop agreeing.
#pragma once

#include <cctype>
#include <string>
#include <string_view>

namespace cml {

inline std::string lower(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    // ponytail: ASCII fold only. FTS5's unicode61 tokenizer folds Cyrillic and accents
    // for the retrieval itself; this is the overlap floor, and both sides of the compare
    // go through this same function, so the two stay consistent.
    for (const char c : s)
        out.push_back(static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    return out;
}

}  // namespace cml
