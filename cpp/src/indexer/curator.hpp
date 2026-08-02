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
#include <string>

#include "paths.hpp"

namespace cml::idx {

inline std::filesystem::path curator_lock() { return data_dir() / "curator.lock"; }

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
    const int devnull = ::open("/dev/null", O_RDWR);
    if (devnull >= 0) {
        ::dup2(devnull, STDIN_FILENO);
        ::dup2(devnull, STDOUT_FILENO);
        ::dup2(devnull, STDERR_FILENO);
        if (devnull > STDERR_FILENO) ::close(devnull);
    }
    const std::string exe = self_exe();
    const std::string lim = std::to_string(limit);
    ::execl(exe.c_str(), exe.c_str(), "distill", "--limit", lim.c_str(), nullptr);
    ::_exit(0);  // exec failed: the lock releases with the process, nothing is stuck
}

}  // namespace cml::idx
