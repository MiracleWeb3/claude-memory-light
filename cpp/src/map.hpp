// map: the whole index as one static HTML file — no server, no build step.
//
// The layout is computed here, not in the browser: every node ships with fixed
// fx/fy/fz, so the page renders 9000 nodes without running a physics simulation.
#pragma once

#include <cstddef>
#include <cstdint>
#include <map>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace cml {

struct MRow {
    std::int64_t rowid = 0;
    std::string role;
    std::size_t pidx = 0;
    bool has_sess = false;
    std::size_t sidx = 0;
    std::string date;
    std::string snip;
    std::string sess8;
    std::string project;
};

struct CodeNode {
    std::string id;
    std::string label;
    std::string snippet;
};

struct MapData {
    std::vector<std::string> proj_names;
    std::vector<std::pair<std::string, std::size_t>> sess_list;  // (session id, project index)
    std::vector<MRow> msgs;
    std::map<std::string, std::size_t> role_counts;
    std::unordered_map<std::string, std::string> note_ids;  // lowercased note name -> "m:<rowid>"
    std::vector<std::pair<std::string, std::vector<std::string>>> pending_wikilinks;
    std::vector<CodeNode> code_nodes;
    std::vector<std::pair<std::string, std::string>> code_edges;
    std::string code_root_label;
    std::string db_mb = "?";
};

struct Payload {
    std::string json;
    std::size_t nodes = 0;
    std::size_t links = 0;
    std::size_t code = 0;
};

// [[wikilink]] target names, trimmed and lowercased — the edges between notes.
std::vector<std::string> wikilinks(std::string_view text);

// Places every node on its shell and serializes the graph the page reads.
Payload build_payload(const MapData& d);

int map(const std::vector<std::string>& args);

}  // namespace cml
