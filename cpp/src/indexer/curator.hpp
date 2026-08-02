// Curation runs detached, because it cannot run any other way.
//
// Measured 2026-08-02 on deepseek-v4-pro: judging five rows took 102 seconds — roughly
// 20s a row, because the curator is a reasoning-tier model. There is no batch size that
// fits inside a Stop hook, so the earlier design of "call it with whatever time is left"
// could only ever time out, curate nothing, and look like a broken key. It was not a
// broken key; the key answers in 2s.
//
// So index_all starts the curator and returns. The turn is never blocked, the work still
// happens, and a lock file keeps one slow curator from becoming twenty.
#pragma once

#include <fcntl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#include <filesystem>
#include <fstream>
#include <string>

#include "paths.hpp"

namespace cml::idx {

inline std::filesystem::path curator_lock() { return data_dir() / "curator.lock"; }
inline std::filesystem::path curator_log() { return data_dir() / "curator.log"; }

// What the curator's last run amounted to, in one line. A detached run has no other
// witness: on 2026-08-02 a key on a spent account returned "Insufficient Balance" every
// single run and nothing anywhere reported it, because this output went to /dev/null.
//
// A failing run still ends with a cheerful "distilled: kept 0, dropped 0", so the LAST
// line is the wrong one to show — it reports the non-event and swallows the cause. A
// complaint outranks the summary; with no complaint, the summary is the whole story.
inline std::string curator_last_line() {
    std::ifstream in(curator_log(), std::ios::binary);
    if (!in) return {};
    std::string line, last, complaint;
    while (std::getline(in, line)) {
        const auto a = line.find_first_not_of(" \t\r\n");
        if (a == std::string::npos) continue;
        last = line.substr(a, line.find_last_not_of(" \t\r\n") - a + 1);
        if (complaint.empty() &&
            (last.find("skipped (") != std::string::npos || last.find("error") != std::string::npos))
            complaint = last;
    }
    return complaint.empty() ? last : complaint;
}

// Path to the running binary, so the child is the same cml the hook just used.
inline std::string self_exe() {
    char buf[4096];
    const ssize_t n = ::readlink("/proc/self/exe", buf, sizeof buf - 1);
    if (n > 0) {
        buf[n] = '\0';
        return buf;
    }
    return "cml";  // fall back to PATH
}

// Double-fork so the curator is reparented to init and outlives the hook that started it.
// Returns true if a curator was started; false if one is already running.
inline bool spawn_curator(int limit = 20) {
    std::error_code ec;
    std::filesystem::create_directories(curator_lock().parent_path(), ec);
    const int lock = ::open(curator_lock().c_str(), O_RDWR | O_CREAT, 0600);
    if (lock < 0) return false;
    if (::flock(lock, LOCK_EX | LOCK_NB) != 0) {  // one is already working
        ::close(lock);
        return false;
    }

    const pid_t first = ::fork();
    if (first < 0) {
        ::close(lock);
        return false;
    }
    if (first > 0) {
        int st = 0;
        ::waitpid(first, &st, 0);  // the intermediate exits at once; no zombie
        ::close(lock);
        return true;
    }

    ::setsid();
    if (::fork() > 0) ::_exit(0);  // intermediate leaves; the grandchild is orphaned to init

    // Grandchild: hold the lock for the whole run, detach the streams, work.
    // Output goes to the log, not /dev/null — see curator_last_line() for why. O_TRUNC
    // keeps only the most recent run, which is the only one anyone asks about, and bounds
    // the file for free.
    const int devnull = ::open("/dev/null", O_RDONLY);
    if (devnull >= 0) {
        ::dup2(devnull, STDIN_FILENO);
        if (devnull > STDERR_FILENO) ::close(devnull);
    }
    const int log = ::open(curator_log().c_str(), O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (log >= 0) {
        ::dup2(log, STDOUT_FILENO);
        ::dup2(log, STDERR_FILENO);
        if (log > STDERR_FILENO) ::close(log);
    }
    const std::string exe = self_exe();
    const std::string lim = std::to_string(limit);
    ::execl(exe.c_str(), exe.c_str(), "distill", "--limit", lim.c_str(), nullptr);
    ::_exit(0);  // exec failed: the lock releases with the process, nothing is stuck
}

}  // namespace cml::idx
