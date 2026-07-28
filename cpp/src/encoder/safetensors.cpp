// safetensors: an 8-byte little-endian header length, a JSON header naming every
// tensor, then the packed data. The file is mapped, never read — the 133 MB of
// bge-small weights would otherwise be copied into a 3.6 GB laptop on every call.

#include <fcntl.h>
#include <simdjson.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cstring>
#include <utility>

#include "encoder/internals.hpp"

namespace cml::enc {

Safetensors::~Safetensors() {
    if (map_) munmap(map_, map_len_);
}

Safetensors::Safetensors(Safetensors&& o) noexcept
    : index_(std::move(o.index_)),
      map_(std::exchange(o.map_, nullptr)),
      map_len_(std::exchange(o.map_len_, 0)),
      base_(std::exchange(o.base_, nullptr)) {}

Safetensors& Safetensors::operator=(Safetensors&& o) noexcept {
    if (this != &o) {
        if (map_) munmap(map_, map_len_);
        index_ = std::move(o.index_);
        map_ = std::exchange(o.map_, nullptr);
        map_len_ = std::exchange(o.map_len_, 0);
        base_ = std::exchange(o.base_, nullptr);
    }
    return *this;
}

Safetensors Safetensors::open(const std::string& path, std::string& error) {
    Safetensors st;
    const int fd = ::open(path.c_str(), O_RDONLY);
    if (fd < 0) {
        error = "cannot open " + path;
        return st;
    }
    struct stat sb {};
    if (fstat(fd, &sb) != 0 || sb.st_size < 8) {
        ::close(fd);
        error = "unreadable safetensors: " + path;
        return st;
    }
    const auto len = static_cast<std::size_t>(sb.st_size);
    void* map = mmap(nullptr, len, PROT_READ, MAP_PRIVATE, fd, 0);
    ::close(fd);
    if (map == MAP_FAILED) {
        error = "cannot mmap " + path;
        return st;
    }
    const auto* bytes = static_cast<const unsigned char*>(map);

    std::uint64_t header_len = 0;
    std::memcpy(&header_len, bytes, 8);
    if (header_len == 0 || header_len > len - 8) {
        munmap(map, len);
        error = "corrupt safetensors header in " + path;
        return st;
    }
    const std::size_t data_start = 8 + static_cast<std::size_t>(header_len);

    simdjson::dom::parser parser;
    simdjson::dom::element header;
    const std::string json(reinterpret_cast<const char*>(bytes + 8),
                           static_cast<std::size_t>(header_len));
    simdjson::dom::object entries;
    if (parser.parse(json).get(header) != simdjson::SUCCESS ||
        header.get(entries) != simdjson::SUCCESS) {
        munmap(map, len);
        error = "unparsable safetensors header in " + path;
        return st;
    }

    for (auto [name, spec] : entries) {
        if (name == "__metadata__") continue;
        std::string_view dtype;
        simdjson::dom::array dims, offs;
        if (spec["dtype"].get(dtype) != simdjson::SUCCESS ||
            spec["shape"].get(dims) != simdjson::SUCCESS ||
            spec["data_offsets"].get(offs) != simdjson::SUCCESS || offs.size() != 2) {
            continue;
        }
        auto it = offs.begin();
        const std::int64_t begin = (*it).get_int64().value_unsafe();
        const std::int64_t end = (*++it).get_int64().value_unsafe();
        if (begin < 0 || end < begin) continue;

        Entry e;
        e.dtype = dtype;
        e.offset = data_start + static_cast<std::size_t>(begin);
        e.count = static_cast<std::size_t>(end - begin) / 4;  // meaningful for F32 only
        for (auto d : dims) e.dims.push_back(d.get_int64().value_unsafe());
        if (e.offset > len || static_cast<std::size_t>(end - begin) > len - e.offset) {
            munmap(map, len);
            error = "tensor '" + std::string(name) + "' runs past the end of " + path;
            return st;
        }
        // A float view of a misaligned address is undefined behaviour, not a slow
        // path. HuggingFace pads the header to keep the data section aligned, so
        // this only ever fires on a genuinely malformed file.
        if (e.dtype == "F32" && (e.offset % alignof(float)) != 0) {
            munmap(map, len);
            error = "tensor '" + std::string(name) + "' is not float-aligned in " + path;
            return st;
        }
        st.index_.emplace(std::string(name), std::move(e));
    }

    if (st.index_.empty()) {
        munmap(map, len);
        error = "safetensors header names no tensors: " + path;
        return st;
    }
    st.map_ = map;
    st.map_len_ = len;
    st.base_ = bytes;
    return st;
}

std::span<const float> Safetensors::get(std::string_view name, std::string& error) const {
    const auto it = index_.find(std::string(name));
    if (it == index_.end()) {
        error = "safetensors has no tensor '" + std::string(name) + "'";
        return {};
    }
    if (it->second.dtype != "F32") {
        error = "tensor '" + std::string(name) + "' is " + it->second.dtype +
                ", and only F32 is read — convert the checkpoint to float32";
        return {};
    }
    return {reinterpret_cast<const float*>(base_ + it->second.offset), it->second.count};
}

std::vector<std::int64_t> Safetensors::shape(std::string_view name) const {
    const auto it = index_.find(std::string(name));
    return it == index_.end() ? std::vector<std::int64_t>{} : it->second.dims;
}

}  // namespace cml::enc
