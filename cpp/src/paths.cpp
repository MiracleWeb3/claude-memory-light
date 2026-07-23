#include "paths.hpp"

#include <chrono>
#include <cstdio>
#include <cstdlib>

namespace cml {

std::filesystem::path home() {
    if (const char* h = std::getenv("HOME")) return std::filesystem::path(h);
    return std::filesystem::path("/");
}

std::filesystem::path data_dir() {
    if (const char* p = std::getenv("CML_HOME")) {
        if (*p) return std::filesystem::path(p);
    }
    return home() / ".claude/claude-memory-light";
}

std::string flatten(std::string_view path) {
    std::string out(path);
    for (char& c : out) {
        if (c == '/' || c == '.') c = '-';
    }
    return out;
}

std::string flat_home() { return flatten(home().string()); }

std::string project_label(std::string_view dirname) {
    const std::string fh = flat_home();
    std::string label;
    if (dirname == fh) {
        label = "home";
    } else if (dirname.size() > fh.size() + 1 && dirname.substr(0, fh.size()) == fh &&
               dirname[fh.size()] == '-') {
        label = std::string(dirname.substr(fh.size() + 1));
    } else {
        std::size_t i = dirname.find_first_not_of('-');
        label = (i == std::string_view::npos) ? std::string() : std::string(dirname.substr(i));
    }
    return label.empty() ? "misc" : label;
}

std::filesystem::path inbox_path_for(std::string_view cwd) {
    return data_dir() / "inbox" / (project_label(flatten(cwd)) + ".md");
}

std::int64_t now_secs() {
    using namespace std::chrono;
    return duration_cast<seconds>(system_clock::now().time_since_epoch()).count();
}

std::string iso_date(std::int64_t secs) {
    return iso_minute(secs).substr(0, 10);
}

std::string iso_minute(std::int64_t secs) {
    using namespace std::chrono;
    // C++20 calendar support replaces the hand-rolled civil-date algorithm the
    // Rust version needed to avoid a chrono crate.
    const sys_seconds tp{seconds{secs}};
    const auto day_point = floor<days>(tp);
    const year_month_day ymd{day_point};
    const hh_mm_ss hms{tp - day_point};

    char buf[48];  // 33 max with a pathological year; fortify wants the slack
    std::snprintf(buf, sizeof buf, "%04d-%02u-%02u %02lld:%02lld",
                  static_cast<int>(ymd.year()),
                  static_cast<unsigned>(ymd.month()),
                  static_cast<unsigned>(ymd.day()),
                  static_cast<long long>(hms.hours().count()),
                  static_cast<long long>(hms.minutes().count()));
    return buf;
}

}  // namespace cml
