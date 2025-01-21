#!/usr/bin/env bash

docker run -v `pwd`:/app --workdir="/app" -it rust bash -c "apt-get update && apt-get install -y libfuse3-dev && cargo build"

# compiles file to target/debug/when-fs
