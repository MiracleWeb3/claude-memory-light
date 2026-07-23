// The smallest JSON writer that keeps the two binaries interchangeable.
//
// simdjson (already linked) reads JSON; it does not write it. Only two commands
// emit any — map's payload and distill's request — and both are fixed-shape, so a
// serializer library would be a dependency for ~40 lines of escaping.
//
// Key order matters here: serde_json without `preserve_order` stores objects in a
// BTreeMap, so the Rust binary emits fields alphabetically. Callers write them in
// that order by hand; this header only escapes.
#pragma once

#include <charconv>
#include <cstdio>
#include <string>
#include <string_view>

namespace cml::json {

// Appends `s` as a quoted JSON string. Matches serde_json: control characters
// escaped, `/` and non-ASCII passed through as-is.
inline void quote_into(std::string& out, std::string_view s) {
    out.push_back('"');
    for (const char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\b': out += "\\b"; break;
            case '\f': out += "\\f"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(c) < 0x20) {
                    char buf[7];
                    std::snprintf(buf, sizeof buf, "\\u%04x", static_cast<unsigned char>(c));
                    out += buf;
                } else {
                    out.push_back(c);
                }
        }
    }
    out.push_back('"');
}

inline std::string quote(std::string_view s) {
    std::string out;
    quote_into(out, s);
    return out;
}

// Shortest round-trip form, with the trailing ".0" serde_json's float writer keeps
// (5 serializes as "5.0"): JS reads both as the same number, but a diff of the two
// binaries' output should show nothing.
inline std::string num(double v) {
    char buf[32];
    const auto [end, ec] = std::to_chars(buf, buf + sizeof buf, v);
    std::string out(buf, end);
    if (ec != std::errc{}) return "0.0";
    if (out.find_first_of(".en") == std::string::npos) out += ".0";
    return out;
}

}  // namespace cml::json
