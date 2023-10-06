#!/bin/bash

set -euxo pipefail

while true; do
  cargo test --all --no-default-features --features libp2p ten_nodes_one_down -- --nocapture
  if [ $? -ne 0 ]; then
    break
  fi
done
