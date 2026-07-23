// One runnable check, no framework. Suites live in selftest/, one concern per
// file; a failing check aborts with the offending case named.

#include <cstdio>

#include "selftest/harness.hpp"

int main() {
    cml_test::suite_filters();
    cml_test::suite_embedding();
    cml_test::suite_curate();
    cml_test::suite_map();
    cml_test::suite_ports();
    std::printf("all %d checks passed\n", cml_test::checks);
    return 0;
}
