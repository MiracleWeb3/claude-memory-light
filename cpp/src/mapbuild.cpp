// Layout and serialization for `cml map`.
//
// Projects sit on a ring around the core, sessions on a sphere around their project,
// messages on a sphere around their session — every position deterministic, so the
// same index always draws the same picture and the browser does zero physics.

#include <algorithm>
#include <cmath>
#include <numbers>
#include <string>
#include <vector>

#include "json.hpp"
#include "map.hpp"
#include "utf8.hpp"

namespace cml {
namespace {

constexpr double kRingR = 300.0;
const double kTau = 2.0 * std::numbers::pi;

struct Vec3 {
    double x = 0.0, y = 0.0, z = 0.0;
};

// i-th of n points on a unit sphere (golden-angle fibonacci lattice).
Vec3 fib_sphere(std::size_t i, std::size_t n) {
    const double nf = static_cast<double>(std::max<std::size_t>(n, 1));
    const double y = 1.0 - 2.0 * (static_cast<double>(i) + 0.5) / nf;
    const double r = std::sqrt(std::max(1.0 - y * y, 0.0));
    const double phi = static_cast<double>(i) * 2.399963229728653;
    return {r * std::cos(phi), y, r * std::sin(phi)};
}

// Deterministic pseudo-random in [-1,1] from a seed — layout jitter without an RNG.
double jit(std::uint64_t seed) {
    const std::uint64_t x = seed * 6364136223846793005ULL + 1442695040888963407ULL;
    return static_cast<double>(static_cast<std::uint32_t>(x >> 33)) / 4294967295.0 * 2.0 - 1.0;
}

void comma(std::string& s) {
    if (!s.empty()) s.push_back(',');
}

// {"fx":..,"fy":..,"fz":..} — the shared tail of every node, in serde_json's
// alphabetical key order.
void xyz_into(std::string& s, const Vec3& p) {
    s += "\"fx\":" + json::num(p.x) + ",\"fy\":" + json::num(p.y) + ",\"fz\":" + json::num(p.z);
}

void link_into(std::string& s, std::string_view source, std::string_view target,
               const char* kind) {
    comma(s);
    s += "{\"kind\":\"";
    s += kind;
    s += "\",\"source\":";
    json::quote_into(s, source);
    s += ",\"target\":";
    json::quote_into(s, target);
    s += '}';
}

}  // namespace

Payload build_payload(const MapData& d) {
    const bool has_code = !d.code_nodes.empty();
    const std::size_t slots = d.proj_names.size() + (has_code ? 1 : 0);
    const auto slot_pos = [slots](std::size_t k) {
        const double th = kTau * static_cast<double>(k) / static_cast<double>(std::max<std::size_t>(slots, 1));
        return Vec3{kRingR * std::cos(th), (static_cast<double>(k % 3) - 1.0) * 46.0,
                    kRingR * std::sin(th)};
    };

    // sessions on shells around their project
    std::vector<std::vector<std::size_t>> sess_by_p(d.proj_names.size());
    for (std::size_t si = 0; si < d.sess_list.size(); ++si) {
        sess_by_p[d.sess_list[si].second].push_back(si);
    }
    std::vector<Vec3> spos(d.sess_list.size());
    for (std::size_t p = 0; p < sess_by_p.size(); ++p) {
        const auto& list = sess_by_p[p];
        const Vec3 base = slot_pos(p);
        const double r = 110.0 + std::sqrt(static_cast<double>(list.size())) * 7.0;
        for (std::size_t j = 0; j < list.size(); ++j) {
            const Vec3 u = fib_sphere(j, list.size());
            spos[list[j]] = {base.x + u.x * r, base.y + u.y * r * 0.72, base.z + u.z * r};
        }
    }

    // Which shell a message hangs on: 0 = its session, 1 = its project (memory
    // notes have no session), 2 = the core (wiki pages).
    const auto key_of = [](const MRow& m) -> std::pair<int, std::size_t> {
        if (m.has_sess) return {0, m.sidx};
        if (m.role == "wiki") return {2, 0};
        return {1, m.pidx};
    };
    std::map<std::pair<int, std::size_t>, std::size_t> order, totals;
    for (const auto& m : d.msgs) ++totals[key_of(m)];

    std::string nodes, links;
    nodes += "{\"fx\":0.0,\"fy\":0.0,\"fz\":0.0,\"group\":\"center\",\"id\":\"center\","
             "\"label\":\"memory\",\"val\":34}";
    std::size_t n_nodes = 1, n_links = 0, n_code = 0;

    for (std::size_t p = 0; p < d.proj_names.size(); ++p) {
        const std::string id = "p:" + d.proj_names[p];
        comma(nodes);
        nodes += '{';
        xyz_into(nodes, slot_pos(p));
        nodes += ",\"group\":\"project\",\"id\":";
        json::quote_into(nodes, id);
        nodes += ",\"label\":";
        json::quote_into(nodes, d.proj_names[p]);
        nodes += ",\"val\":16}";
        link_into(links, "center", id, "spine");
        ++n_nodes;
        ++n_links;
    }

    for (std::size_t si = 0; si < d.sess_list.size(); ++si) {
        const auto& [sid, p] = d.sess_list[si];
        const std::string id = "s:" + sid;
        comma(nodes);
        nodes += '{';
        xyz_into(nodes, spos[si]);
        nodes += ",\"group\":\"session\",\"id\":";
        json::quote_into(nodes, id);
        nodes += ",\"label\":";
        json::quote_into(nodes, utf8_take(sid, 8));
        nodes += ",\"val\":7}";
        link_into(links, "p:" + d.proj_names[p], id, "spine");
        ++n_nodes;
        ++n_links;
    }

    for (const auto& m : d.msgs) {
        const auto k = key_of(m);
        const std::size_t idx = order[k]++;
        const std::size_t n = totals[k];
        Vec3 base;
        double r = 140.0;
        std::string parent = "center";
        if (k.first == 0) {
            base = spos[k.second];
            // The one place the two binaries' output is not byte-identical: Rust's
            // f64::cbrt is not glibc's cbrt, and they disagree by 1 ULP on most
            // inputs (n=2: ...728b vs ...728c). It moves a dot by ~1e-13 units, so
            // it is left alone rather than vendoring musl's cbrt to chase it.
            r = 26.0 + std::cbrt(static_cast<double>(n)) * 7.0;
            parent = "s:" + d.sess_list[k.second].first;
        } else if (k.first == 1) {
            base = slot_pos(k.second);
            r = 78.0;
            parent = "p:" + d.proj_names[k.second];
        }
        const Vec3 u = fib_sphere(idx, n);
        const auto s = static_cast<std::uint64_t>(m.rowid);
        const Vec3 pos{base.x + u.x * r + jit(s) * 4.0, base.y + u.y * r + jit(s ^ 0xA5A5) * 4.0,
                       base.z + u.z * r + jit(s ^ 0x5A5A) * 4.0};
        const std::string id = "m:" + std::to_string(m.rowid);

        comma(nodes);
        nodes += '{';
        xyz_into(nodes, pos);
        nodes += ",\"group\":";
        json::quote_into(nodes, m.role);
        nodes += ",\"id\":";
        json::quote_into(nodes, id);
        nodes += ",\"label\":";
        json::quote_into(nodes, m.date);
        nodes += ",\"project\":";
        json::quote_into(nodes, m.project);
        nodes += ",\"session\":";
        json::quote_into(nodes, m.sess8);
        nodes += ",\"snippet\":";
        json::quote_into(nodes, m.snip);
        nodes += ",\"ts\":";
        json::quote_into(nodes, m.date);
        nodes += ",\"val\":";
        nodes += (m.role == "memory" || m.role == "wiki") ? "5.0" : "1.6";
        nodes += '}';
        link_into(links, parent, id, "leaf");
        ++n_nodes;
        ++n_links;
    }

    for (const auto& [from, targets] : d.pending_wikilinks) {
        for (const auto& name : targets) {
            const auto it = d.note_ids.find(name);
            if (it != d.note_ids.end() && it->second != from) {
                link_into(links, from, it->second, "wikilink");
                ++n_links;
            }
        }
    }

    if (has_code) {
        const std::string root = "code:" + d.code_root_label;
        const Vec3 c = slot_pos(slots - 1);
        comma(nodes);
        nodes += '{';
        xyz_into(nodes, c);
        nodes += ",\"group\":\"coderoot\",\"id\":";
        json::quote_into(nodes, root);
        nodes += ",\"label\":\"code\",\"val\":18}";
        link_into(links, "center", root, "spine");
        ++n_nodes;
        ++n_links;

        const std::size_t cn = d.code_nodes.size();
        const double cr = 60.0 + std::cbrt(static_cast<double>(cn)) * 9.0;
        for (std::size_t i = 0; i < cn; ++i) {
            const Vec3 u = fib_sphere(i, cn);
            comma(nodes);
            nodes += '{';
            xyz_into(nodes, {c.x + u.x * cr, c.y + u.y * cr, c.z + u.z * cr});
            nodes += ",\"group\":\"code\",\"id\":";
            json::quote_into(nodes, "c:" + d.code_nodes[i].id);
            nodes += ",\"label\":";
            json::quote_into(nodes, d.code_nodes[i].label);
            nodes += ",\"project\":\"code\",\"session\":\"\",\"snippet\":";
            json::quote_into(nodes, d.code_nodes[i].snippet);
            nodes += ",\"ts\":\"\",\"val\":2.4}";
            link_into(links, root, "c:" + d.code_nodes[i].id, "tether");
            ++n_nodes;
            ++n_links;
            ++n_code;
        }
        for (const auto& [s, t] : d.code_edges) {
            link_into(links, "c:" + s, "c:" + t, "code");
            ++n_links;
        }
    }

    std::string roles;
    for (const auto& [role, count] : d.role_counts) {
        comma(roles);
        json::quote_into(roles, role);
        roles += ':' + std::to_string(count);
    }

    std::string out = "{\"links\":[" + links + "],\"nodes\":[" + nodes + "],\"stats\":{\"db_mb\":";
    json::quote_into(out, d.db_mb);
    out += ",\"projects\":" + std::to_string(d.proj_names.size()) + ",\"roles\":{" + roles +
           "},\"rows\":" + std::to_string(d.msgs.size()) +
           ",\"sessions\":" + std::to_string(d.sess_list.size()) + "}}";

    // "</" would close the inline <script> if a snippet contains it
    for (std::size_t i = out.find("</"); i != std::string::npos; i = out.find("</", i + 3)) {
        out.insert(i + 1, "\\");
    }
    return {std::move(out), n_nodes, n_links, n_code};
}

}  // namespace cml
