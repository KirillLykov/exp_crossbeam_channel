#!/usr/bin/env bash

# Need to install `libc6-dbg`:
#
# sudo apt install libc6-dbg
#

export RUSTFLAGS="-C debuginfo=2 -C force-frame-pointers=yes"
export CFLAGS="-fno-omit-frame-pointer -g"
export CXXFLAGS="-fno-omit-frame-pointer -g"

cargo build --profile release-with-debug 
