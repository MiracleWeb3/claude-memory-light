#include "wordpiece.hpp"

#include <simdjson.h>

#include <algorithm>

#include "unicode.hpp"

namespace cml {

WordPiece WordPiece::load(const std::string& path, std::string& error) {
    WordPiece wp;
    simdjson::dom::parser parser;
    simdjson::dom::element doc;
    if (parser.load(path).get(doc) != simdjson::SUCCESS) {
        error = "cannot read tokenizer.json at " + path;
        return wp;
    }

    simdjson::dom::element model;
    if (doc["model"].get(model) != simdjson::SUCCESS) {
        error = "tokenizer.json has no model section";
        return wp;
    }

    std::string_view sv;
    if (model["continuing_subword_prefix"].get(sv) == simdjson::SUCCESS) wp.prefix_ = sv;
    std::int64_t n = 0;
    if (model["max_input_chars_per_word"].get(n) == simdjson::SUCCESS && n > 0)
        wp.max_chars_per_word_ = static_cast<std::size_t>(n);

    std::string unk = "[UNK]";
    if (model["unk_token"].get(sv) == simdjson::SUCCESS) unk = sv;

    simdjson::dom::object vocab;
    if (model["vocab"].get(vocab) != simdjson::SUCCESS) {
        error = "tokenizer.json has no vocab";
        return wp;
    }

    std::vector<std::size_t> lens;
    lens.reserve(vocab.size());
    wp.vocab_.reserve(vocab.size() * 2);
    for (auto [token, id] : vocab) {
        std::int64_t v = 0;
        if (id.get(v) != simdjson::SUCCESS) continue;
        wp.vocab_.emplace(std::string(token), static_cast<std::uint32_t>(v));
        lens.push_back(token.size());
    }
    if (wp.vocab_.empty()) {
        error = "tokenizer vocab is empty";
        return wp;
    }

    // Median token length, matching model2vec's truncation hack exactly.
    std::sort(lens.begin(), lens.end());
    wp.median_token_bytes_ = lens[lens.size() / 2];
    if (wp.median_token_bytes_ == 0) wp.median_token_bytes_ = 1;

    const auto it = wp.vocab_.find(unk);
    if (it == wp.vocab_.end()) {
        error = "tokenizer claims unk_token='" + unk + "' but it is not in the vocab";
        wp.vocab_.clear();
        return wp;
    }
    wp.unk_id_ = it->second;
    return wp;
}

std::string WordPiece::normalize(std::string_view text) const {
    // BertNormalizer with clean_text, handle_chinese_chars, strip_accents and
    // lowercase all enabled (strip_accents is null in the file, which HF resolves
    // to the value of lowercase).
    const auto cps = uni::decode(text);
    std::vector<uni::Cp> out;
    out.reserve(cps.size() + 16);

    for (uni::Cp c : cps) {
        if (c == 0 || c == 0xFFFD || uni::is_control(c)) continue;  // clean_text
        if (uni::is_space(c)) {
            out.push_back(' ');
            continue;
        }
        if (uni::is_cjk(c)) {  // handle_chinese_chars: isolate each ideograph
            out.push_back(' ');
            out.push_back(c);
            out.push_back(' ');
            continue;
        }
        out.push_back(uni::strip_accent(uni::lower(c)));
    }
    return uni::encode(out);
}

std::vector<std::string> WordPiece::pre_tokenize(std::string_view normalized) const {
    // BertPreTokenizer: split on whitespace, then break out each punctuation char.
    std::vector<std::string> words;
    std::string cur;
    const auto flush = [&] {
        if (!cur.empty()) {
            words.push_back(cur);
            cur.clear();
        }
    };
    for (const uni::Cp c : uni::decode(normalized)) {
        if (uni::is_space(c)) {
            flush();
        } else if (uni::is_punct(c)) {
            flush();
            std::string p;
            uni::encode_one(c, p);
            words.push_back(std::move(p));
        } else {
            uni::encode_one(c, cur);
        }
    }
    flush();
    return words;
}

std::vector<std::uint32_t> WordPiece::encode_word(const std::string& word) const {
    // Greedy longest-match-first, the WordPiece algorithm.
    if (uni::decode(word).size() > max_chars_per_word_) return {unk_id_};

    std::vector<std::uint32_t> ids;
    std::size_t start = 0;
    while (start < word.size()) {
        std::size_t end = word.size();
        bool matched = false;
        while (end > start) {
            // Never split inside a multi-byte character. `end` is an index one past
            // the piece, so only inspect it when it addresses a real byte.
            while (end > start && end < word.size() &&
                   (static_cast<unsigned char>(word[end]) & 0xC0) == 0x80) {
                --end;
            }
            if (end <= start) break;  // nothing left to try; the word is unknown

            std::string piece = word.substr(start, end - start);
            if (start > 0) piece = prefix_ + piece;
            const auto it = vocab_.find(piece);
            if (it != vocab_.end()) {
                ids.push_back(it->second);
                start = end;
                matched = true;
                break;
            }
            --end;  // guarded above: end > start here, so this cannot underflow
        }
        if (!matched) return {unk_id_};  // whole word is unknown, per WordPiece
    }
    return ids;
}

std::vector<std::uint32_t> WordPiece::encode(std::string_view text) const {
    std::vector<std::uint32_t> ids;
    if (!ok()) return ids;
    for (const auto& word : pre_tokenize(normalize(text))) {
        for (const std::uint32_t id : encode_word(word)) ids.push_back(id);
    }
    return ids;
}

}  // namespace cml
