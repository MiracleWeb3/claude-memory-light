# Point an existing cml installation at the build just made, so the hooks stop running
# yesterday's binary. Deliberately does nothing when cml is not already installed:
# building a repo must never be how software appears in someone's ~/.claude.
if(DEFINED ENV{CML_HOME})
  set(bin_dir "$ENV{CML_HOME}/bin")
else()
  set(bin_dir "$ENV{HOME}/.claude/claude-memory-light/bin")
endif()

if(NOT EXISTS "${bin_dir}/cml")
  return()
endif()

# Copy to a sibling then rename, never write in place: the installed binary may be
# executing right now (the Stop hook runs it every turn), and writing into a running
# executable fails with ETXTBSY while replacing the directory entry never does.
execute_process(COMMAND "${CMAKE_COMMAND}" -E copy "${CML_BIN}" "${bin_dir}/cml.new"
                RESULT_VARIABLE copied)
if(copied EQUAL 0)
  execute_process(COMMAND "${CMAKE_COMMAND}" -E rename "${bin_dir}/cml.new" "${bin_dir}/cml")
  message(STATUS "cml: refreshed ${bin_dir}/cml")
endif()
