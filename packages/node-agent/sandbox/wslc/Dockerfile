FROM alpine:3.21

LABEL org.opencontainers.image.title="Coding Tools MCP WSLC Sandbox"
LABEL org.opencontainers.image.description="Alpine toolchain image for Coding Tools MCP WSLC sandboxes"

RUN apk add --no-cache \
      bash \
      build-base \
      ca-certificates \
      cargo \
      cmake \
      curl \
      git \
      go \
      jq \
      libffi-dev \
      linux-headers \
      nodejs \
      npm \
      openssh-client \
      openssl-dev \
      pkgconf \
      py3-pip \
      python3 \
      python3-dev \
      rust \
      sqlite-dev \
      zlib-dev \
  && ln -sf /usr/bin/python3 /usr/local/bin/python \
  && ln -sf /usr/bin/pip3 /usr/local/bin/pip

# Fail the image build if Alpine ever stops satisfying the sandbox contract.
RUN python3 -c "import sys; assert sys.version_info[:2] == (3, 12), sys.version" \
  && node -e "const major=Number(process.versions.node.split('.')[0]); if (major < 22) { throw new Error('Node.js 22+ required, found ' + process.version); }" \
  && printf '#include <stdio.h>\nint main(void){puts("c-ok");return 0;}\n' > /tmp/ctmcp.c \
  && cc /tmp/ctmcp.c -o /tmp/ctmcp-c \
  && /tmp/ctmcp-c \
  && printf '#include <iostream>\nint main(){std::cout << "cpp-ok\\n";}\n' > /tmp/ctmcp.cpp \
  && c++ /tmp/ctmcp.cpp -o /tmp/ctmcp-cpp \
  && /tmp/ctmcp-cpp \
  && printf 'fn main(){println!("rust-ok");}\n' > /tmp/ctmcp.rs \
  && rustc /tmp/ctmcp.rs -o /tmp/ctmcp-rust \
  && /tmp/ctmcp-rust \
  && printf 'package main\nimport "fmt"\nfunc main(){fmt.Println("go-ok")}\n' > /tmp/ctmcp.go \
  && go build -o /tmp/ctmcp-go /tmp/ctmcp.go \
  && /tmp/ctmcp-go \
  && rm -f /tmp/ctmcp.c /tmp/ctmcp.cpp /tmp/ctmcp.rs /tmp/ctmcp.go /tmp/ctmcp-c /tmp/ctmcp-cpp /tmp/ctmcp-rust /tmp/ctmcp-go

WORKDIR /workspace

CMD ["/bin/sh"]
