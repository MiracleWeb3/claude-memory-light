// BertNormalizer + BertPreTokenizer + WordPiece, matching the HuggingFace
// tokenizer.json that ships with the model2vec model.
//
// No special tokens are ever added: model2vec pools raw content tokens, and a
// [CLS]/[SEP] pair would drag every vector toward the same two rows.
#pragma once

#include <cstdint>
#include <string>
#include <string_view>
#include <unordered_map>
#include <vector>

namespace cml {

class WordPiece {
public:
    // Parses a HuggingFace tokenizer.json. On failure returns an object for which
    // ok() is false and fills `error`.
    static WordPiece load(const std::string& tokenizer_json_path, std::string& error);

    bool ok() const { return !vocab_.empty(); }
    std::uint32_t unk_id() const { return unk_id_; }

    // Median vocabulary token length in bytes — model2vec's pre-truncation hack.
    std::size_t median_token_bytes() const { return median_token_bytes_; }

    std::vector<std::uint32_t> encode(std::string_view text) const;

    // Exposed for the self-test: the normalizer and splitter are where a port
    // silently diverges, and a wrong vector is invisible without checking them.
    std::string normalize(std::string_view text) const;
    std::vector<std::string> pre_tokenize(std::string_view normalized) const;

private:
    std::vector<std::uint32_t> encode_word(const std::string& word) const;

    std::unordered_map<std::string, std::uint32_t> vocab_;
    std::string prefix_ = "##";
    std::uint32_t unk_id_ = 0;
    std::size_t max_chars_per_word_ = 100;
    std::size_t median_token_bytes_ = 1;
};

}  // namespace cml
