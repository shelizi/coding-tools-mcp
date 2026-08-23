# Coding Tools MCP WSLC sandbox image

This image is the Alpine development toolchain used by the Node Agent WSLC sandbox.

It contains:

- Python 3.12 plus pip and Python headers
- Node.js 22+ plus npm
- Rust plus Cargo
- Go
- GCC/G++, make, CMake, pkg-config, Linux headers, OpenSSL/SQLite/zlib/libffi development headers
- Git, SSH, curl, jq and Bash

The Dockerfile includes build-time smoke tests for Python, Node.js, C, C++, Rust and Go. The Node Agent builds this image inside its managed WSLC session the first time the built-in image is required, then reuses the image stored in that session VHDX.

Manual build in the current WSLC session:

```powershell
wslc build --pull -t coding-tools-mcp/wslc-sandbox:alpine-3.21 .\packages\node-agent\sandbox\wslc
```

Manual verification:

```powershell
wslc run --rm coding-tools-mcp/wslc-sandbox:alpine-3.21 sh -lc 'python3 --version; node --version; npm --version; rustc --version; cargo --version; go version; cc --version | head -1; c++ --version | head -1'
```

Command containers still default to the sandbox network setting (`none`). Only first-time image provisioning needs registry/package network access.
