#!/usr/bin/env bash
# Builds the Steinberg SDK example plugins the VST3 integration tests load, and prints the
# directory to point BOOTHCRAFT_VST3_TEST_DIR at.
#
#   scripts/build_vst3_fixtures.sh [work-dir]      (default: target/vst3-fixtures/work)
#   BOOTHCRAFT_VST3_TEST_DIR=<printed dir> \
#     cargo test -p boothcraft-plugin-vst3 --features integration --test integration
#
# Needs git and cmake. The SDK is MIT-licensed (3.8+); it is cloned into the work directory and
# never vendored into this repository. (The SDK's own `again` and `host-checker` examples need
# VSTGUI and are not used.)
set -euo pipefail
work="${1:-$(cd "$(dirname "$0")/.." && pwd)/target/vst3-fixtures/work}"
tag="v3.8.0_build_66"
mkdir -p "$work"
cd "$work"
if [ ! -d sdk/.git ]; then
  git clone --depth 1 --branch "$tag" --recurse-submodules --shallow-submodules \
    https://github.com/steinbergmedia/vst3sdk.git sdk
fi
cmake -S sdk -B build -DCMAKE_BUILD_TYPE=Release \
  -DSMTG_ENABLE_VST3_PLUGIN_EXAMPLES=ON -DSMTG_ENABLE_VST3_HOSTING_EXAMPLES=OFF \
  -DSMTG_ENABLE_VSTGUI_SUPPORT=OFF -DSMTG_CREATE_PLUGIN_LINK=OFF -DSMTG_RUN_VST_VALIDATOR=OFF \
  > configure.log 2>&1 || { tail -20 configure.log; exit 1; }
cmake --build build --config Release --target adelay again-sample-accurate sync-delay -j 8 \
  > build.log 2>&1 || { tail -20 build.log; exit 1; }
echo "$work/build/VST3/Release"
