// The whole test harness: a counter and an abort-on-fail check. Suites live one
// concern per file; the entry point (../selftest.cpp) runs them in order.
#pragma once

#include <cstdio>
#include <cstdlib>

namespace cml_test {

inline int checks = 0;

inline void ok(bool cond, const char* what) {
    ++checks;
    if (!cond) {
        std::fprintf(stderr, "FAIL: %s\n", what);
        std::abort();
    }
}

void suite_filters();    // noise, correction flag, paths, digest, squeeze
void suite_embedding();  // tokenizer + vectors (skips without the model cache)
void suite_curate();     // forget --match, judge request/verdicts
void suite_map();        // wikilinks + map payload
void suite_ports();      // 2.5.0: chronic loops + prompt hints

}  // namespace cml_test
