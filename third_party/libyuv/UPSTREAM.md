# libyuv Source

Source mirror: https://github.com/lemenkov/libyuv

Pinned revision: `1ddb18fb6b6cc91f852648a6e64d2dee31b11a80`.

Original project: https://chromium.googlesource.com/libyuv/libyuv/

The `source/` and `include/` directories are unmodified upstream files. License, patent grant, and authors are included in `LICENSE`, `PATENTS`, and `AUTHORS`. JPEG support is excluded. The Windows build uses the x64 kernels with runtime SIMD dispatch.

macOS arm64 also builds the NEON kernels. `rust/build.rs` generates a temporary copy of `row_neon64.cc` with Clang `target("i8mm")` attributes on its four I8MM functions. This lets Apple Clang assemble those kernels without raising the baseline CPU requirement of the other functions; upstream runtime feature dispatch remains in effect. The vendored files themselves are unchanged.
