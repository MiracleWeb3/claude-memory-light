// map: collects the index, hands it to the layout, writes one self-contained page.
//
// three.js, OrbitControls and app.js are vendored and baked into the binary, so the
// output opens from a file:// URL on a machine with no network and no node_modules.
// Rust does that with include_str!; C++ has no such thing before C++23's #embed, so
// the assembler does it (see mapassets.S.in) — no 1.3 MB string literal for the
// compiler to chew through.

#include <unistd.h>

#include <simdjson.h>

#include <algorithm>
#include <cctype>
#include <charconv>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <optional>
#include <string>
#include <vector>

#include "db.hpp"
#include "map.hpp"
#include "paths.hpp"
#include "utf8.hpp"

// Bounds of each .incbin'd asset. The pointers come from the same section, so the
// difference is the byte length.
extern "C" const char cml_map_html[], cml_map_html_end[], cml_three_js[], cml_three_js_end[],
    cml_controls_js[], cml_controls_js_end[], cml_app_js[], cml_app_js_end[];

namespace cml {
namespace {

namespace fs = std::filesystem;

constexpr std::size_t kCodeCap = 3000;

std::string_view asset(const char* begin, const char* end) {
    return {begin, static_cast<std::size_t>(end - begin)};
}

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(),
                   [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return s;
}

void replace_all(std::string& s, std::string_view needle, std::string_view with) {
    for (std::size_t i = s.find(needle); i != std::string::npos;
         i = s.find(needle, i + with.size())) {
        s.replace(i, needle.size(), with);
    }
}

}  // namespace

// No regex needed: two literal delimiters and a trim.
std::vector<std::string> wikilinks(std::string_view text) {
    std::vector<std::string> out;
    std::string_view rest = text;
    for (;;) {
        const auto a = rest.find("[[");
        if (a == std::string_view::npos) break;
        rest.remove_prefix(a + 2);
        const auto b = rest.find("]]");
        if (b == std::string_view::npos) break;
        std::string_view name = rest.substr(0, b);
        const auto lead = name.find_first_not_of(" \t\r\n");
        if (lead != std::string_view::npos) {
            name = name.substr(lead, name.find_last_not_of(" \t\r\n") - lead + 1);
            std::string lowered = lower(std::string(name));
            if (!lowered.empty() && lowered.size() < 100) out.push_back(std::move(lowered));
        }
        rest.remove_prefix(b + 2);
    }
    return out;
}

namespace {

void collect(Db& db, std::size_t limit, MapData& d) {
    const auto gists = gist_lookup(db);
    std::unordered_map<std::string, std::size_t> proj_of, sess_of;

    Stmt s(db, "SELECT rowid, role, project, session, ts, text FROM mem ORDER BY rowid DESC LIMIT ?1");
    s.bind(1, static_cast<std::int64_t>(limit));
    while (s.step()) {
        MRow m;
        m.rowid = s.i64(0);
        m.role = s.text(1);
        m.project = s.text(2);
        const std::string session = s.text(3);
        const std::string ts = s.text(4);
        const std::string text = s.text(5);

        // wiki pages hang off the core directly — registering their pseudo-project
        // would put an empty planet on the ring
        if (m.role != "wiki") {
            const auto [it, fresh] = proj_of.emplace(m.project, d.proj_names.size());
            if (fresh) d.proj_names.push_back(m.project);
            m.pidx = it->second;
        }
        if (m.role != "memory" && m.role != "wiki") {
            const auto [it, fresh] = sess_of.emplace(session, d.sess_list.size());
            if (fresh) d.sess_list.emplace_back(session, m.pidx);
            m.has_sess = true;
            m.sidx = it->second;
        }
        ++d.role_counts[m.role];

        const std::string mid = "m:" + std::to_string(m.rowid);
        if (m.role == "memory" || m.role == "wiki") {
            d.note_ids[lower(session)] = mid;
            auto found = wikilinks(text);
            if (!found.empty()) d.pending_wikilinks.emplace_back(mid, std::move(found));
        }
        const auto g = gists.find(stable_key(session, ts, m.role, text));
        m.snip = (g != gists.end()) ? g->second : utf8_take(text, 320);
        m.date = (ts.size() >= 10) ? utf8_take(ts, 10) : "";
        m.sess8 = utf8_take(session, 8);
        d.msgs.push_back(std::move(m));
    }
}

void load_code_graph(const fs::path& cp, MapData& d) {
    simdjson::dom::parser parser;
    simdjson::dom::element g;
    if (const auto e = parser.load(cp.string()).get(g); e != simdjson::SUCCESS) {
        std::fprintf(stderr, "cml: could not read code graph %s: %s\n", cp.string().c_str(),
                     simdjson::error_message(e));
        return;
    }
    d.code_root_label = "repo";
    std::error_code ec;
    const fs::path cwd = fs::current_path(ec);
    if (!ec && !cwd.filename().empty()) d.code_root_label = cwd.filename().string();

    std::unordered_set<std::string> kept;
    simdjson::dom::array gnodes;
    if (g["nodes"].get(gnodes) == simdjson::SUCCESS) {
        for (auto n : gnodes) {
            if (d.code_nodes.size() >= kCodeCap) break;
            std::string_view id;
            if (n["id"].get(id) != simdjson::SUCCESS) continue;
            std::string_view label, ftype, src;
            if (n["label"].get(label) != simdjson::SUCCESS) label = id;
            if (n["file_type"].get(ftype) != simdjson::SUCCESS) ftype = "code";
            if (n["source_file"].get(src) != simdjson::SUCCESS) src = "";
            kept.emplace(id);
            d.code_nodes.push_back({std::string(id), std::string(label),
                                    std::string(label) + "\n[" + std::string(ftype) + "] " +
                                        std::string(src)});
        }
        if (gnodes.size() > kCodeCap) {
            std::printf("code graph capped at %zu of %zu nodes\n", kCodeCap, gnodes.size());
        }
    }
    simdjson::dom::array gedges;
    if (g["edges"].get(gedges) != simdjson::SUCCESS && g["links"].get(gedges) != simdjson::SUCCESS) {
        return;
    }
    for (auto e : gedges) {
        std::string_view src, tgt;
        if (e["source"].get(src) != simdjson::SUCCESS || e["target"].get(tgt) != simdjson::SUCCESS) {
            continue;
        }
        if (kept.count(std::string(src)) && kept.count(std::string(tgt))) {
            d.code_edges.emplace_back(src, tgt);
        }
    }
}

// Rust's Command::spawn: fire and forget, never wait for the browser.
void open_in_browser(const fs::path& p) {
    if (fork() != 0) return;
    const std::string path = p.string();
    char* argv[] = {const_cast<char*>("xdg-open"), const_cast<char*>(path.c_str()), nullptr};
    execvp(argv[0], argv);
    _exit(127);
}

}  // namespace

int map(const std::vector<std::string>& args) {
    std::size_t limit = 6000;
    bool open_page = true, no_code = false;
    std::optional<fs::path> code_path;
    for (std::size_t i = 0; i < args.size(); ++i) {
        if (args[i] == "--limit") {
            limit = 6000;
            if (++i < args.size()) {
                std::size_t parsed = 0;
                const char* end = args[i].data() + args[i].size();
                if (std::from_chars(args[i].data(), end, parsed).ptr == end) limit = parsed;
            }
        } else if (args[i] == "--no-open") {
            open_page = false;
        } else if (args[i] == "--no-code") {
            no_code = true;
        } else if (args[i] == "--code" && ++i < args.size()) {
            code_path = args[i];
        }
    }
    if (!code_path && !no_code) {
        const fs::path autop = "graphify-out/graph.json";
        std::error_code ec;
        if (fs::is_regular_file(autop, ec)) code_path = autop;
    }

    Db db = open_db();
    if (!db) {
        std::fprintf(stderr, "cml: cannot open index\n");
        return 1;
    }

    MapData d;
    collect(db, limit, d);
    if (code_path) load_code_graph(*code_path, d);

    const fs::path dbfile = data_dir() / "index.db";
    std::error_code ec;
    if (const auto sz = fs::file_size(dbfile, ec); !ec) {
        char mb[24];
        std::snprintf(mb, sizeof mb, "%.1f", static_cast<double>(sz) / 1e6);
        d.db_mb = mb;
    }

    const Payload payload = build_payload(d);
    std::string html(asset(cml_map_html, cml_map_html_end));
    replace_all(html, "/*%%PAYLOAD%%*/ null", payload.json);
    replace_all(html, "/*%%THREE%%*/", asset(cml_three_js, cml_three_js_end));
    replace_all(html, "/*%%CONTROLS%%*/", asset(cml_controls_js, cml_controls_js_end));
    replace_all(html, "/*%%APP%%*/", asset(cml_app_js, cml_app_js_end));

    const fs::path out = data_dir() / "map.html";
    {
        std::ofstream f(out, std::ios::binary);
        if (!f) {
            std::fprintf(stderr, "cml: cannot write %s\n", out.string().c_str());
            return 1;
        }
        f << html;
    }
    std::printf("map: %s (%zu nodes, %zu links, %zu code, %.1f MB)\n", out.string().c_str(),
                payload.nodes, payload.links, payload.code,
                static_cast<double>(html.size()) / 1e6);
    if (open_page) open_in_browser(out);
    return 0;
}

}  // namespace cml
