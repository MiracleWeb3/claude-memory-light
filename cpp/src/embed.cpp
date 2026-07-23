#include "embed.hpp"

#include <fcntl.h>
#include <simdjson.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <cmath>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <utility>

#include "paths.hpp"
#include "utf8.hpp"

namespace fs = std::filesystem;

namespace cml {
namespace {

constexpr std::size_t kMaxTokens = 512;  // model2vec's encode() default

// "minishlab/potion-base-8M" -> the snapshot folder inside the HuggingFace cache.
fs::path resolve_model_dir(const std::string& id) {
    std::error_code ec;
    if (fs::is_directory(id, ec)) return id;

    std::string flat = "models--" + id;
    for (auto& c : flat) {
        if (c == '/') c = '-';
    }
    // "models--minishlab-potion-base-8M" -> the double dash HF actually uses
    const std::size_t slash = id.find('/');
    if (slash != std::string::npos) {
        flat = "models--" + id.substr(0, slash) + "--" + id.substr(slash + 1);
    }

    fs::path cache;
    if (const char* h = std::getenv("HF_HOME")) {
        cache = fs::path(h) / "hub";
    } else {
        cache = home() / ".cache/huggingface/hub";
    }
    const fs::path snapshots = cache / flat / "snapshots";
    if (!fs::is_directory(snapshots, ec)) return {};
    for (const auto& e : fs::directory_iterator(snapshots, ec)) {
        if (e.is_directory() && fs::exists(e.path() / "model.safetensors", ec)) return e.path();
    }
    return {};
}

}  // namespace

std::string embed_model_id() {
    if (const char* m = std::getenv("CML_EMBED_MODEL")) {
        if (*m) return m;
    }
    return "minishlab/potion-base-8M";
}

StaticModel::~StaticModel() {
    if (map_) munmap(map_, map_len_);
}

StaticModel::StaticModel(StaticModel&& o) noexcept
    : tokenizer_(std::move(o.tokenizer_)),
      matrix_(std::exchange(o.matrix_, nullptr)),
      map_(std::exchange(o.map_, nullptr)),
      map_len_(std::exchange(o.map_len_, 0)),
      rows_(std::exchange(o.rows_, 0)),
      dim_(std::exchange(o.dim_, 0)),
      normalize_(o.normalize_) {}

StaticModel& StaticModel::operator=(StaticModel&& o) noexcept {
    if (this != &o) {
        if (map_) munmap(map_, map_len_);
        tokenizer_ = std::move(o.tokenizer_);
        matrix_ = std::exchange(o.matrix_, nullptr);
        map_ = std::exchange(o.map_, nullptr);
        map_len_ = std::exchange(o.map_len_, 0);
        rows_ = std::exchange(o.rows_, 0);
        dim_ = std::exchange(o.dim_, 0);
        normalize_ = o.normalize_;
    }
    return *this;
}

StaticModel StaticModel::load(const std::string& id, std::string& error) {
    StaticModel m;
    const fs::path dir = resolve_model_dir(id);
    if (dir.empty()) {
        error = "embedding model '" + id +
                "' not found in the HuggingFace cache — fetch it once with network access";
        return m;
    }

    m.tokenizer_ = WordPiece::load((dir / "tokenizer.json").string(), error);
    if (!m.tokenizer_.ok()) return m;

    // config.json decides whether vectors are L2-normalised.
    {
        simdjson::dom::parser parser;
        simdjson::dom::element cfg;
        if (parser.load((dir / "config.json").string()).get(cfg) == simdjson::SUCCESS) {
            bool nrm = true;
            if (cfg["normalize"].get(nrm) == simdjson::SUCCESS) m.normalize_ = nrm;
        }
    }

    const std::string weights = (dir / "model.safetensors").string();
    const int fd = ::open(weights.c_str(), O_RDONLY);
    if (fd < 0) {
        error = "cannot open " + weights;
        return m;
    }
    struct stat st {};
    if (fstat(fd, &st) != 0 || st.st_size < 8) {
        ::close(fd);
        error = "unreadable safetensors: " + weights;
        return m;
    }
    const auto len = static_cast<std::size_t>(st.st_size);
    void* map = mmap(nullptr, len, PROT_READ, MAP_PRIVATE, fd, 0);
    ::close(fd);
    if (map == MAP_FAILED) {
        error = "cannot mmap " + weights;
        return m;
    }

    // safetensors: u64 header length, then a JSON header, then packed tensor data.
    const auto* bytes = static_cast<const unsigned char*>(map);
    std::uint64_t header_len = 0;
    std::memcpy(&header_len, bytes, 8);
    if (header_len == 0 || 8 + header_len > len) {
        munmap(map, len);
        error = "corrupt safetensors header in " + weights;
        return m;
    }

    simdjson::dom::parser hp;
    simdjson::dom::element header;
    const std::string header_json(reinterpret_cast<const char*>(bytes + 8),
                                  static_cast<std::size_t>(header_len));
    if (hp.parse(header_json).get(header) != simdjson::SUCCESS) {
        munmap(map, len);
        error = "unparsable safetensors header in " + weights;
        return m;
    }

    simdjson::dom::element t;
    std::string_view dtype;
    simdjson::dom::array shape, offs;
    if (header["embeddings"].get(t) != simdjson::SUCCESS ||
        t["dtype"].get(dtype) != simdjson::SUCCESS || t["shape"].get(shape) != simdjson::SUCCESS ||
        t["data_offsets"].get(offs) != simdjson::SUCCESS) {
        munmap(map, len);
        error = "safetensors has no 'embeddings' tensor";
        return m;
    }
    if (dtype != "F32") {
        munmap(map, len);
        error = "embeddings dtype is " + std::string(dtype) + ", expected F32";
        return m;
    }

    std::vector<std::int64_t> dims;
    for (auto d : shape) dims.push_back(d.get_int64().value_unsafe());
    std::vector<std::int64_t> off;
    for (auto d : offs) off.push_back(d.get_int64().value_unsafe());
    if (dims.size() != 2 || off.size() != 2) {
        munmap(map, len);
        error = "unexpected embeddings shape";
        return m;
    }

    m.rows_ = static_cast<std::size_t>(dims[0]);
    m.dim_ = static_cast<std::size_t>(dims[1]);
    const std::size_t start = 8 + header_len + static_cast<std::size_t>(off[0]);
    const std::size_t need = m.rows_ * m.dim_ * sizeof(float);
    if (start + need > len) {
        munmap(map, len);
        error = "embeddings tensor runs past end of file";
        return m;
    }

    m.map_ = map;
    m.map_len_ = len;
    m.matrix_ = reinterpret_cast<const float*>(bytes + start);
    return m;
}

std::vector<float> StaticModel::encode(std::string_view text) const {
    std::vector<float> sum(dim_, 0.0F);
    if (!ok()) return sum;

    // model2vec pre-truncates by characters before tokenising: max_tokens * the
    // median vocabulary token length.
    const std::string clipped =
        utf8_take(text, kMaxTokens * tokenizer_.median_token_bytes());

    auto ids = tokenizer_.encode(clipped);
    const std::uint32_t unk = tokenizer_.unk_id();
    ids.erase(std::remove(ids.begin(), ids.end(), unk), ids.end());
    if (ids.size() > kMaxTokens) ids.resize(kMaxTokens);

    std::size_t count = 0;
    for (const std::uint32_t id : ids) {
        if (id >= rows_) continue;
        const float* row = matrix_ + static_cast<std::size_t>(id) * dim_;
        for (std::size_t i = 0; i < dim_; ++i) sum[i] += row[i];
        ++count;
    }

    const float denom = static_cast<float>(std::max<std::size_t>(count, 1));
    for (float& v : sum) v /= denom;

    if (normalize_) {
        float norm = 0.0F;
        for (const float v : sum) norm += v * v;
        norm = std::max(std::sqrt(norm), 1e-12F);
        for (float& v : sum) v /= norm;
    }
    return sum;
}

}  // namespace cml
