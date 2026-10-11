#!/usr/bin/env bash
# Enforces the workspace's unsafe-code policy.
#
# `[workspace.lints.rust] unsafe_code = "forbid"` cannot be relaxed from inside a crate (E0453),
# so the few crates that must call FFI (plugin hosting backends and native editor windows) opt
# out of `[lints] workspace = true` and copy the workspace lints by hand with `deny`, allowing
# unsafe per module. Every other crate must inherit the workspace lints unchanged.
set -euo pipefail
cd "$(dirname "$0")/.."

FFI_CRATES=" boothcraft-plugin-vst3 boothcraft-plugin-au boothcraft-plugin-window boothcraft-vst3-test-plugin "
fail=0
for manifest in crates/*/Cargo.toml; do
  name=$(basename "$(dirname "$manifest")")
  if [[ "$FFI_CRATES" == *" $name "* ]]; then
    if ! grep -Eq '^unsafe_code *= *"deny"' "$manifest"; then
      echo "error: $name must set unsafe_code = \"deny\" in its own [lints.rust]" >&2
      fail=1
    fi
    if grep -Eq '^workspace *= *true' "$manifest"; then
      echo "error: $name inherits the workspace lints, which forbid unsafe code" >&2
      fail=1
    fi
  else
    if ! awk '/^\[lints\]/{f=1;next} /^\[/{f=0} f && /^workspace *= *true/{ok=1} END{exit !ok}' "$manifest"; then
      echo "error: $name must have [lints] workspace = true" >&2
      fail=1
    fi
    if grep -Eq 'unsafe_code *= *"(allow|deny)"' "$manifest"; then
      echo "error: $name relaxes unsafe_code; only FFI crates may (${FFI_CRATES})" >&2
      fail=1
    fi
  fi
done
exit $fail
