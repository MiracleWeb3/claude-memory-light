cmake_minimum_required(VERSION 3.20)
project(cml CXX C)  # C for the vendored sqlite-vec extension

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

add_compile_options(-Wall -Wextra -Wpedantic)

# map's page assets (three.js, OrbitControls, app.js, the HTML shell) are embedded
# by the assembler's .incbin instead of a generated string literal — see
# src/mapassets.S.in. They are read from the Rust source tree, never copied.
enable_language(ASM)
set(CML_ASSET_DIR "${CMAKE_CURRENT_SOURCE_DIR}/assets")
configure_file(src/mapassets.S.in "${CMAKE_CURRENT_BINARY_DIR}/mapassets.S" @ONLY)
set_source_files_properties("${CMAKE_CURRENT_BINARY_DIR}/mapassets.S" PROPERTIES
  OBJECT_DEPENDS "${CML_ASSET_DIR}/map.html;${CML_ASSET_DIR}/app.js;${CML_ASSET_DIR}/vendor/three.module.js;${CML_ASSET_DIR}/vendor/OrbitControls.js")

# Only two libraries, both already on the system, both C at the core:
#   sqlite3  - FTS5 index (Rust needed rusqlite to reach it; C++ calls it directly)
#   simdjson - transcript parsing; the corpus is ~531 MB of JSONL, so the parser
#              is the hot path and gets the fastest credible option.
find_library(SQLITE3_LIB sqlite3 REQUIRED)
find_library(SIMDJSON_LIB simdjson REQUIRED)

# Vendored, not fetched: sqlite-vec is one C file, so it is compiled straight in
# rather than resolved through a package manager at build time.
add_library(sqlite_vec STATIC vendor/sqlite-vec.c)
target_include_directories(sqlite_vec PUBLIC vendor)
target_compile_definitions(sqlite_vec PRIVATE SQLITE_CORE=1)
# Third-party C: do not hold it to our warning bar.
target_compile_options(sqlite_vec PRIVATE -w)

add_library(cml_core STATIC
  src/paths.cpp
  src/noise.cpp
  src/work.cpp
  src/db.cpp
  src/transcript.cpp
  src/indexer.cpp
  src/search.cpp
  src/report.cpp
  src/wordpiece.cpp
  src/embed.cpp
  src/vec.cpp
  src/forget.cpp
  src/distill.cpp
  src/loops.cpp
  src/hint.cpp
  src/map.cpp
  src/mapbuild.cpp
  "${CMAKE_CURRENT_BINARY_DIR}/mapassets.S"
)
target_include_directories(cml_core PUBLIC src)
target_link_libraries(cml_core PUBLIC sqlite_vec)
target_link_libraries(cml_core PUBLIC ${SQLITE3_LIB} ${SIMDJSON_LIB})

add_executable(cml src/main.cpp src/capture.cpp)
target_link_libraries(cml PRIVATE cml_core)

# One runnable check, no framework: asserts that fail loudly on a broken filter.
add_executable(cml_selftest
  src/selftest.cpp
  src/selftest/filters.cpp
  src/selftest/embedding.cpp
  src/selftest/curate.cpp
  src/selftest/map.cpp
  src/selftest/ports.cpp
)
target_link_libraries(cml_selftest PRIVATE cml_core)
enable_testing()
add_test(NAME selftest COMMAND cml_selftest)
