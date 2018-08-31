#!/bin/bash -e
cargo build --all
cargo test --all
if [ "$TRAVIS_RUST_VERSION" = nightly ]; then
  cargo bench --all
fi
