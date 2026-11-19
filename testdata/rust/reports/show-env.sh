export LLVM_PROFILE_FILE='/REPO/target/rust-%p-%18m.profraw'
export __CARGO_LLVM_COV_RUSTC_WRAPPER=1
export __CARGO_LLVM_COV_RUSTC_WRAPPER_RUSTFLAGS='-Cinstrument-coverage--cfg=coverage--cfg=coverage_nightly'
export __CARGO_LLVM_COV_RUSTC_WRAPPER_CRATE_NAMES=calc,calc_tests,calc
export RUSTC_WRAPPER=/HOME/.cargo/bin/cargo-llvm-cov
export CARGO_LLVM_COV=1
export CARGO_LLVM_COV_SHOW_ENV=1
export CARGO_LLVM_COV_TARGET_DIR=/REPO/target
export CARGO_LLVM_COV_BUILD_DIR=/REPO/target
