// UTF-8 length and prefix, counted in characters.
//
// This is not pedantry: SQLite's substr() on TEXT counts characters, and the dedup
// key is built from substr(text,1,64). Counting bytes here would produce keys that
// disagree with the ones already in the database, and every row would look new.
#pragma once

#include <cstddef>
#include <string>
#include <string_view>

namespace cml {

// Continuation bytes are 10xxxxxx; every other byte starts a character.
inline bool is_utf8_lead(unsigned char c) { return (c & 0xC0) != 0x80; }

inline std::size_t utf8_len(std::string_view s) {
    std::size_t n = 0;
    for (const char c : s) {
        if (is_utf8_lead(static_cast<unsigned char>(c))) ++n;
    }
    return n;
}

// First `chars` characters, never splitting a multi-byte sequence.
inline std::string utf8_take(std::string_view s, std::size_t chars) {
    std::size_t n = 0, i = 0;
    for (; i < s.size(); ++i) {
        if (is_utf8_lead(static_cast<unsigned char>(s[i]))) {
            if (n == chars) break;
            ++n;
        }
    }
    return std::string(s.substr(0, i));
}

}  // namespace cml
