// Minimal Unicode helpers for the BERT normalizer: decode/encode UTF-8, lowercase,
// strip accents, and classify punctuation / CJK / control characters.
//
// ponytail: covers ASCII, Latin-1, Latin Extended-A, Greek and Cyrillic — every script
// that actually appears in this corpus. It is NOT a full Unicode implementation: a
// codepoint outside those ranges passes through unchanged instead of being case-folded.
// Upgrade path is ICU, at the cost of a 30 MB dependency for a rounding error in recall.
#pragma once

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace cml::uni {

using Cp = std::uint32_t;

// Decode UTF-8 into codepoints. Invalid bytes become U+FFFD so a corrupt transcript
// can never desynchronise the tokenizer.
inline std::vector<Cp> decode(std::string_view s) {
    std::vector<Cp> out;
    out.reserve(s.size());
    for (std::size_t i = 0; i < s.size();) {
        const auto b0 = static_cast<unsigned char>(s[i]);
        Cp cp = 0xFFFD;
        std::size_t n = 1;
        if (b0 < 0x80) {
            cp = b0;
        } else if ((b0 & 0xE0) == 0xC0 && i + 1 < s.size()) {
            cp = ((b0 & 0x1Fu) << 6) | (static_cast<unsigned char>(s[i + 1]) & 0x3Fu);
            n = 2;
        } else if ((b0 & 0xF0) == 0xE0 && i + 2 < s.size()) {
            cp = ((b0 & 0x0Fu) << 12) | ((static_cast<unsigned char>(s[i + 1]) & 0x3Fu) << 6) |
                 (static_cast<unsigned char>(s[i + 2]) & 0x3Fu);
            n = 3;
        } else if ((b0 & 0xF8) == 0xF0 && i + 3 < s.size()) {
            cp = ((b0 & 0x07u) << 18) | ((static_cast<unsigned char>(s[i + 1]) & 0x3Fu) << 12) |
                 ((static_cast<unsigned char>(s[i + 2]) & 0x3Fu) << 6) |
                 (static_cast<unsigned char>(s[i + 3]) & 0x3Fu);
            n = 4;
        }
        out.push_back(cp);
        i += n;
    }
    return out;
}

inline void encode_one(Cp cp, std::string& out) {
    if (cp < 0x80) {
        out.push_back(static_cast<char>(cp));
    } else if (cp < 0x800) {
        out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
        out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else {
        out.push_back(static_cast<char>(0xF0 | (cp >> 18)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
        out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
}

inline std::string encode(const std::vector<Cp>& cps) {
    std::string out;
    out.reserve(cps.size());
    for (const Cp cp : cps) encode_one(cp, out);
    return out;
}

inline Cp lower(Cp c) {
    if (c >= 'A' && c <= 'Z') return c + 32;
    if (c >= 0x00C0 && c <= 0x00DE && c != 0x00D7) return c + 32;   // Latin-1 caps
    if (c >= 0x0100 && c <= 0x017F) return (c % 2 == 0) ? c + 1 : c; // Latin Ext-A pairs
    if (c >= 0x0391 && c <= 0x03A9) return c + 32;                   // Greek
    if (c >= 0x0410 && c <= 0x042F) return c + 32;                   // Cyrillic А-Я
    if (c >= 0x0400 && c <= 0x040F) return c + 80;                   // Cyrillic Ѐ-Џ
    return c;
}

// BertNormalizer strips accents when `strip_accents` is null and lowercase is on:
// NFD-decompose, then drop the combining marks. Rather than carry a full NFD table,
// map each precomposed letter straight to its base — the observable result is the
// same, because every combining mark would be discarded immediately afterwards.
//
// Cyrillic is NOT optional here. The model's vocab contains и and е but not й or ё,
// so leaving those composed turns every Russian word containing them into [UNK] —
// which the pooler then discards, quietly degrading the vector.
inline Cp strip_accent(Cp c) {
    // Latin-1 Supplement, lowercase range.
    if (c >= 0x00E0 && c <= 0x00FF) {
        static constexpr char kLatin1[] = "aaaaaaaceeeeiiii\0nooooo\0ouuuuy\0y";
        const char b = kLatin1[c - 0x00E0];
        return b ? static_cast<Cp>(b) : c;
    }
    // Latin Extended-A: every entry is a precomposed Latin letter.
    if (c >= 0x0100 && c <= 0x017F) {
        static constexpr char kLatinA[] =
            "aaaaaaccccccccddddeeeeeeeeeegggggggghhhh"
            "iiiiiiiiiiijjjjkkkllllllllllnnnnnnnnnoooo"
            "ooooeerrrrrrssssssssttttttuuuuuuuuuuuuww"
            "yyyzzzzzzs";
        const std::size_t i = c - 0x0100;
        return (i < sizeof(kLatinA) - 1) ? static_cast<Cp>(kLatinA[i]) : c;
    }
    // Cyrillic precomposed letters, lowercase (uppercase is folded before this runs).
    switch (c) {
        case 0x0439: return 0x0438;  // й -> и
        case 0x0451: return 0x0435;  // ё -> е
        case 0x0450: return 0x0435;  // ѐ -> е
        case 0x045D: return 0x0438;  // ѝ -> и
        case 0x045E: return 0x0443;  // ў -> у
        case 0x0453: return 0x0433;  // ѓ -> г
        case 0x045C: return 0x043A;  // ќ -> к
        default: return c;
    }
}

inline bool is_control(Cp c) {
    if (c == '\t' || c == '\n' || c == '\r') return false;  // treated as whitespace
    return c < 0x20 || c == 0x7F || (c >= 0x80 && c <= 0x9F);
}

inline bool is_space(Cp c) {
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == 0x0B || c == 0x0C ||
           c == 0x00A0 || c == 0x2028 || c == 0x2029 || (c >= 0x2000 && c <= 0x200A) ||
           c == 0x3000;
}

// BERT's definition: ASCII punctuation plus the Unicode punctuation blocks.
inline bool is_punct(Cp c) {
    if ((c >= 33 && c <= 47) || (c >= 58 && c <= 64) || (c >= 91 && c <= 96) ||
        (c >= 123 && c <= 126))
        return true;
    return (c >= 0x2010 && c <= 0x2027) || (c >= 0x2030 && c <= 0x205E) ||
           (c >= 0x00A1 && c <= 0x00BF && c != 0x00AA && c != 0x00B5 && c != 0x00BA) ||
           (c >= 0x3001 && c <= 0x303F) || (c >= 0xFF01 && c <= 0xFF0F);
}

inline bool is_cjk(Cp c) {
    return (c >= 0x4E00 && c <= 0x9FFF) || (c >= 0x3400 && c <= 0x4DBF) ||
           (c >= 0x20000 && c <= 0x2A6DF) || (c >= 0x2A700 && c <= 0x2B73F) ||
           (c >= 0x2B740 && c <= 0x2B81F) || (c >= 0x2B820 && c <= 0x2CEAF) ||
           (c >= 0xF900 && c <= 0xFAFF) || (c >= 0x2F800 && c <= 0x2FA1F);
}

}  // namespace cml::uni
