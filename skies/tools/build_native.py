"""Package Big Skies (Horizon Skies) as a native Horizon mod (`.hnmod`), for upstream Horizon's launcher.

    python skies/tools/build_native.py [--out skies/dist/big_skies.hnmod] [--bundle DIR]

The mod's own code (skies/mod/src and its shader) goes in unchanged, on a native build of the mod SDK
(skies/native/engine/horizon_sdk.rs), driven by the glue in skies/native through Horizon's native hooks: the method
of Orbital Laser's native build (github.com/Stink-O/orbital-laser, MIT License), whose glue, SDK and post pass these
start from. `--bundle DIR` also writes the unpacked bundle (`DIR/big_skies/`), for `horizon-mixins apply --mods DIR` or
the launcher's `native-build DIR`.

The bundle pins the upstream Horizon commit it was built and tested on (HORIZON below): it uses the game's own
internals (the world generator's biomes for the climate), not only the stable hook points.
"""
import argparse, json, re, shutil, sys, zipfile
from pathlib import Path

# The upstream Horizon commit the bundle is made for (Nightly 2026-11-08, tag nightly-source/2026-11-08-fdc87ae) and
# the horizon-mixins commit that Nightly's launcher builds with (its native_mods::MIXINS_REV).
HORIZON = "fdc87aecdbd116bf1e08adcd062fcbaea8c2e2ca"
MIXINS = "5a64af938ec86b00d53d16a408aabc0038fb3e57"

HERE = Path(__file__).resolve().parent.parent
MOD = HERE / "mod"
NATIVE = HERE / "native"

ap = argparse.ArgumentParser()
ID = "big_skies"
ap.add_argument("--out", default=str(HERE / "dist" / f"{ID}.hnmod"))
ap.add_argument("--bundle", help="also write the unpacked bundle into this folder (as DIR/big_skies/)")
args = ap.parse_args()

version = re.search(r'(?m)^version\s*=\s*"([^"]+)"', (MOD / "Cargo.toml").read_text(encoding="utf-8")).group(1)

# The engine crate's lib.rs: the mod's own, with the native SDK standing in for the WebAssembly one.
lib = (MOD / "src" / "lib.rs").read_text(encoding="utf-8")
docs = re.match(r"(?:\s*//![^\n]*\n)*", lib).group(0)
SDK = """#![allow(dead_code, hidden_glob_reexports, clippy::all)]
// The native build (skies/tools/build_native.py): this crate is its own `horizon_mod_sdk`, the SDK's modules and
// types from horizon_sdk.rs, so the mod's code below builds unchanged.
extern crate self as horizon_mod_sdk;
pub mod horizon_sdk;
pub use horizon_sdk::*;

"""
shader = 'include_str!("../shaders/skies.wgsl")'
if lib.count(shader) != 1:
    sys.exit("lib.rs no longer includes ../shaders/skies.wgsl: update build_native.py")
lib = docs + SDK + lib[len(docs):].replace(shader, 'include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/skies.wgsl"))')

CARGO = f"""[package]
name = "horizon-skies"
version = "{version}"
edition = "2024"
description = "Big Skies for the native build: the mod's code, unchanged, on a native Horizon mod SDK."
license = "MIT"

[dependencies]
log = "0.4"
"""

NOTICE = """Big Skies, native build.

The native method, and the glue (glue.rs), the native mod SDK (engine/src/horizon_sdk.rs) and the post pass
(post.rs) this bundle starts from, are Orbital Laser's native build:
https://github.com/Stink-O/orbital-laser, MIT License, Copyright (c) 2026 Stink-O.
"""

# path in the bundle -> text
files = {
    "horizon-mixin.toml": (NATIVE / "horizon-mixin.toml").read_text(encoding="utf-8")
    .replace("@VERSION@", version).replace("@HORIZON@", HORIZON),
    "glue.rs": (NATIVE / "glue.rs").read_text(encoding="utf-8").replace("@VERSION@", version),
    "post.rs": (NATIVE / "post.rs").read_text(encoding="utf-8"),
    "NOTICE": NOTICE,
    "engine/Cargo.toml": CARGO,
    "engine/NOTICE": NOTICE,
    "engine/src/lib.rs": lib,
    "engine/src/horizon_sdk.rs": (NATIVE / "engine" / "horizon_sdk.rs").read_text(encoding="utf-8"),
    "engine/data/skies.wgsl": (MOD / "shaders" / "skies.wgsl").read_text(encoding="utf-8"),
}
for helper in sorted((NATIVE / "hooks").glob("*.rs")):
    files[helper.name] = helper.read_text(encoding="utf-8")
for src in sorted((MOD / "src").glob("*.rs")):
    if src.name != "lib.rs":
        files[f"engine/src/{src.name}"] = src.read_text(encoding="utf-8")

manifest = {"schema": 1, "id": ID, "version": version, "game": "26.3", "horizon": HORIZON, "mixins": MIXINS}

out = Path(args.out)
out.parent.mkdir(parents=True, exist_ok=True)
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("horizon-native.json", json.dumps(manifest, indent=2) + "\n")
    for path, text in files.items():
        z.writestr(f"mixins/{ID}/{path}", text)
print(f"wrote {out} (Big Skies {version}, native, {len(files)} files, for Horizon {HORIZON[:9]})")

if args.bundle:
    root = Path(args.bundle) / ID
    if root.exists():
        shutil.rmtree(root)
    for path, text in files.items():
        p = root / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text, encoding="utf-8")
    (Path(args.bundle) / "horizon-native.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"wrote the bundle {root}")
