// Markdown notes -> one row each.
//
// Memory files and wiki pages are indexed whole: they are already the distilled thing,
// so there is no per-entry judgement to make the way transcripts.cpp has to.
#include <fstream>
#include <sstream>
#include <string>

#include "files.hpp"
#include "paths.hpp"

namespace cml::idx {

Counts index_md_dir(Db& db, const fs::path& dir, const char* role, const std::string& project,
                    bool force) {
    Counts total;
    std::error_code ec;
    if (!fs::is_directory(dir, ec)) return total;

    for (const auto& f : fs::directory_iterator(dir, ec)) {
        const fs::path path = f.path();
        if (path.extension() != ".md") continue;
        FileMeta fm;
        if (!meta_of(path, fm)) continue;
        const std::string pstr = path.string();
        if (!force && unchanged(db, pstr, fm)) continue;

        std::ifstream in(path);
        if (!in) continue;
        std::ostringstream buf;
        buf << in.rdbuf();
        const std::string text = buf.str();

        db.exec("BEGIN");
        drop_rows_for_file(db, pstr);
        if (!text.empty() && text.find_first_not_of(" \t\r\n") != std::string::npos) {
            Stmt ins(db,
                     "INSERT INTO mem(text, role, project, session, ts, file) "
                     "VALUES(?1,?2,?3,?4,?5,?6)");
            ins.bind(1, text).bind(2, role).bind(3, project)
               .bind(4, path.stem().string()).bind(5, iso_date(fm.mtime)).bind(6, pstr);
            if (ins.run()) ++total.rows;
        }
        upsert_file(db, pstr, fm);
        db.exec("COMMIT");
        ++total.files;
    }
    return total;
}

}  // namespace cml::idx
