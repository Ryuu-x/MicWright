cmake_minimum_required(VERSION 3.20)
project(mw_apo CXX)

set(CMAKE_CXX_STANDARD 17)
# Static CRT (matches .cargo/config.toml +crt-static): audiodg must not need the VC++ redistributable.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")

set(MW_RUST_TARGET_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../target/release" CACHE PATH "cargo --release output dir")

add_library(mw_apo SHARED MwApo.cpp dllmain.cpp mw_apo.def)
target_compile_definitions(mw_apo PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN NOMINMAX)
target_compile_options(mw_apo PRIVATE /W4 /permissive- /EHsc /wd4815)
target_link_libraries(mw_apo PRIVATE
    "${MW_RUST_TARGET_DIR}/mw_dsp_ffi.lib"
    audiobaseprocessingobject audiomediatypecrt audioeng legacy_stdio_definitions
    ole32 advapi32
    # native libs required by the Rust std (cargo rustc -- --print native-static-libs)
    kernel32 ntdll userenv ws2_32 bcrypt synchronization dbghelp)

# audiomediatypecrt.lib names atls.lib as a default lib, but no ATL symbols are needed.
target_link_options(mw_apo PRIVATE /NODEFAULTLIB:atls.lib)

add_executable(mw_apo_test_host test_host.cpp)
target_compile_definitions(mw_apo_test_host PRIVATE UNICODE _UNICODE)
target_link_libraries(mw_apo_test_host PRIVATE audiomediatypecrt ole32 legacy_stdio_definitions)
target_link_options(mw_apo_test_host PRIVATE /NODEFAULTLIB:atls.lib)
