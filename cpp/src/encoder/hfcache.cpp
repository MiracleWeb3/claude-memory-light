// Getting the model files onto disk: locate the HuggingFace snapshot dir, fetch the
// three files we need with curl when they are missing, and read the special token
// ids out of tokenizer.json.

#include <simdjson.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cstdio>
#include <cstdlib>

#include "encoder/internals.hpp"
#include "paths.hpp"

namespace fs = std::filesystem;

namespace cml::enc {
namespace {

const char* const kFiles[] = {"config.json", "tokenizer.json", "model.safetensors"};

fs::path hub_dir() {
    if (const char* h = std::getenv("HF_HOME")) return fs::path(h) / "hub";
    return home() / ".cache/huggingface/hub";
}

// "BAAI/bge-small-en-v1.5" -> "models--BAAI--bge-small-en-v1.5"
std::string flat_repo(const std::string& id) {
    const std::size_t slash = id.find('/');
    if (slash == std::string::npos) return "models--" + id;
    return "models--" + id.substr(0, slash) + "--" + id.substr(slash + 1);
}

// An already-populated snapshot for this repo, or empty. Duplicates embed.cpp's
// private resolver; both should collapse into paths.cpp once that file is free.
fs::path existing_snapshot(const std::string& id) {
    std::error_code ec;
    const fs::path snapshots = hub_dir() / flat_repo(id) / "snapshots";
    if (!fs::is_directory(snapshots, ec)) return {};
    for (const auto& e : fs::directory_iterator(snapshots, ec)) {
        if (e.is_directory() && fs::exists(e.path() / "model.safetensors", ec)) return e.path();
    }
    return {};
}

// Downloaded to a .part and renamed, so an interrupted fetch cannot leave a
// truncated model.safetensors that every later run would happily map.
bool fetch(const std::string& url, const fs::path& dest) {
    const fs::path part = dest.string() + ".part";
    const std::string p = part.string();
    const pid_t pid = fork();
    if (pid < 0) return false;
    if (pid == 0) {
        const char* argv[] = {"curl", "-sfL",         "--retry", "2", "--max-time",
                              "900",  "-o",           p.c_str(), url.c_str(), nullptr};
        execvp(argv[0], const_cast<char* const*>(argv));
        _exit(127);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    std::error_code ec;
    const auto size = fs::file_size(part, ec);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0 || ec || size == 0) {
        fs::remove(part, ec);
        return false;
    }
    fs::rename(part, dest, ec);
    return !ec;
}

}  // namespace

fs::path ensure_model(const std::string& id, std::string& error) {
    std::error_code ec;
    if (fs::is_directory(id, ec)) return id;

    fs::path dir = existing_snapshot(id);
    if (dir.empty()) dir = hub_dir() / flat_repo(id) / "snapshots" / "main";

    std::vector<std::string> missing;
    for (const char* f : kFiles) {
        if (!fs::exists(dir / f, ec)) missing.push_back(f);
    }
    if (missing.empty()) return dir;

    fs::create_directories(dir, ec);
    if (ec) {
        error = "cannot create " + dir.string() + ": " + ec.message();
        return {};
    }
    // Said out loud: this is a ~130 MB download, and a silent one inside an
    // indexing hook looks like a hang.
    std::fprintf(stderr, "cml: fetching %s (%zu file(s)) from HuggingFace...\n", id.c_str(),
                 missing.size());
    for (const auto& f : missing) {
        if (!fetch("https://huggingface.co/" + id + "/resolve/main/" + f, dir / f)) {
            error = "cannot fetch " + f + " for '" + id + "' — needs network access, or curl";
            // A typo'd model id used to leave three empty directories in the user's
            // HuggingFace cache. remove() is the non-recursive one, so it declines
            // to touch anything that turned out to hold files, and the walk stops
            // at the hub root rather than eating empty parents above it.
            const fs::path hub = hub_dir();
            for (fs::path p = dir; p != hub && fs::remove(p, ec); p = p.parent_path()) {
            }
            return {};
        }
    }
    return dir;
}

bool special_ids(const std::string& tokenizer_json, std::uint32_t& cls, std::uint32_t& sep) {
    simdjson::dom::parser parser;
    simdjson::dom::element doc;
    if (parser.load(tokenizer_json).get(doc) != simdjson::SUCCESS) return false;
    simdjson::dom::array added;
    if (doc["added_tokens"].get(added) != simdjson::SUCCESS) return false;

    bool got_cls = false, got_sep = false;
    for (auto t : added) {
        std::string_view content;
        std::int64_t id = 0;
        if (t["content"].get(content) != simdjson::SUCCESS) continue;
        if (t["id"].get(id) != simdjson::SUCCESS || id < 0) continue;
        if (content == "[CLS]") {
            cls = static_cast<std::uint32_t>(id);
            got_cls = true;
        } else if (content == "[SEP]") {
            sep = static_cast<std::uint32_t>(id);
            got_sep = true;
        }
    }
    return got_cls && got_sep;
}

}  // namespace cml::enc
