# libopus (fixed point), for voice notes.
#
# The source tree comes from nixpkgs through FENNEC_OPUS_SRC, which the
# flake's development shell and nix/firmware.nix set; no network access is
# needed. Only the source lists and its CMake helper are taken from libopus,
# its own build is not used.

set(OPUS "$ENV{FENNEC_OPUS_SRC}")
if(NOT EXISTS "${OPUS}/opus_sources.mk")
    # Builds without the `opus` feature need no libopus (build.rs checks the
    # opposite case).
    idf_component_register()
    return()
endif()

include("${OPUS}/cmake/OpusFunctions.cmake")
get_opus_sources(SILK_SOURCES "${OPUS}/silk_sources.mk" silk_sources)
get_opus_sources(CELT_SOURCES "${OPUS}/celt_sources.mk" celt_sources)
get_opus_sources(OPUS_SOURCES "${OPUS}/opus_sources.mk" opus_sources)
# Fixed point: on the ESP32 the float build was measured much slower (36 ms
# per 20 ms frame against 20 ms, both at -O2) and needs twice the memory,
# although the chip has an FPU.
get_opus_sources(SILK_SOURCES_FIXED "${OPUS}/silk_sources.mk" silk_arith)

set(srcs "")
foreach(source ${silk_sources} ${silk_arith} ${celt_sources} ${opus_sources})
    list(APPEND srcs "${OPUS}/${source}")
endforeach()

idf_component_register(
    SRCS ${srcs}
    INCLUDE_DIRS "${OPUS}/include"
    PRIV_INCLUDE_DIRS "${OPUS}/celt" "${OPUS}/silk" "${OPUS}/silk/fixed" "${OPUS}/src"
)

target_compile_definitions(${COMPONENT_LIB} PRIVATE
    OPUS_BUILD
    FIXED_POINT
    DISABLE_FLOAT_API
    # Scratch buffers on the stack (C99 variable length arrays) rather than
    # a global pseudo-stack.
    VAR_ARRAYS
    HAVE_LRINT
    HAVE_LRINTF
)
target_compile_options(${COMPONENT_LIB} PRIVATE -O3 -Wno-unused-function -Wno-maybe-uninitialized)
