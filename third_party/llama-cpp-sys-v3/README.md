# llama-cpp-sys-v3

Raw FFI bindings for [llama.cpp](https://github.com/ggml-org/llama.cpp) with support for **runtime DLL loading**.

## Overview

Unlike traditional sys crates that link against a static or dynamic library at compile-time, this crate resolves symbols at runtime using `libloading`. This allows you to distribute a single Rust binary that can load different hardware-accelerated backends (CPU, CUDA, Vulkan, etc.) on the fly.

## Usage

This crate is intended to be used as a backend for higher-level wrappers like `llama-cpp-v3`. If you want to use it directly:

```rust
use llama_cpp_sys_v3::LlamaLib;

// Load the main DLL (it will also try to resolve ggml.dll)
let lib = LlamaLib::open("path/to/llama.dll")?;

// Access symbols via the lib object
unsafe {
    (lib.symbols.llama_backend_init)();
}
```

## Maintenance

The FFI structures in `src/types.rs` are manually aligned with the modern `llama.cpp` ABI (v3+). If you are using a significantly different version of `llama.cpp`, you may need to update the struct definitions to match the C++ headers.

## License

MIT
