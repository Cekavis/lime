# llama-cpp-v3

Safe and ergonomic Rust wrapper for [llama.cpp](https://github.com/ggml-org/llama.cpp) with **runtime dynamic loading**.

## Features

- 🚀 **Runtime Backend Switching**: Switch between CPU, CUDA, Vulkan, and SYCL without recompiling.
- 📦 **Zero-Configuration Build**: No need for a C++ compiler or `llama.cpp` source locally.
- 🛡️ **Safe API**: RAII-style wrappers for models, contexts, and samplers.
- 🔄 **Latest llama.cpp**: Support for the modern GGUF and vocabulary APIs.
- 🤖 **Agent Support**: Native integration with [llama-cpp-v3-agent-sdk](../llama-cpp-v3-agent-sdk/) for tool-use and multi-agent workflows.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
llama-cpp-v3 = "0.1.7" # Example version
```

## Quick Start

```rust
use llama_cpp_v3::{LlamaBackend, LlamaModel, LlamaContext, LlamaSampler, LoadOptions, Backend};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Load an explicitly packaged backend library
    let backend = LlamaBackend::load(LoadOptions {
        explicit_path: std::path::Path::new("./llama.dll"),
    })?;

    // 2. Load a model
    let model = LlamaModel::load_from_file(&backend, "tinyllama.gguf", LlamaModel::default_params(&backend))?;

    // 3. Create a context
    let mut ctx = LlamaContext::new(&model, LlamaContext::default_params(&model))?;

    // 4. Tokenize and generate
    let tokens = model.tokenize("Hello, my name is", true, true)?;
    // ... fill batch and decode ...

    Ok(())
}
```

## How it Works

This crate uses `llama-cpp-sys-v3` to dynamically load `llama.dll` (Windows) or `libllama.so` (Linux).
The caller is responsible for packaging and selecting the library; the wrapper never performs
network access or downloads native code at runtime.

## License

MIT
