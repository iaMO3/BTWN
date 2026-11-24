"""Assemble the notices from locked Cargo packages and the Windows release toolchain."""
import argparse
import json
import urllib.request
from functools import cache
from pathlib import Path

def is_notice(name):
    return name.lower().startswith(("license", "licence", "copying", "copyright", "unlicense", "notice")) or name in {
        "Hack-Regular.txt", "UFL.txt", "OFL.txt", "emoji-icon-font-mit-license.txt"
    }

@cache
def upstream(repository, revision):
    repo = "/".join(repository.removeprefix("https://github.com/").strip("/").split("/")[:2])
    api = f"https://api.github.com/repos/{repo}/contents?ref={revision}"
    with urllib.request.urlopen(api, timeout=25) as response:
        files = json.load(response)
    result = {}
    for item in files:
        if item["type"] == "file" and is_notice(item["name"]):
            with urllib.request.urlopen(item["download_url"], timeout=25) as response:
                result[item["name"]] = response.read()
    return result

def main():
    args = argparse.ArgumentParser()
    for name in ["metadata","out","wix","rust","mingw"]:
        args.add_argument("--"+name, type=Path, required=True)
    opt = args.parse_args()
    packages = json.loads(opt.metadata.read_text(encoding="utf-8-sig"))["packages"]
    with opt.out.open("w", encoding="utf-8") as out:
        def add(name, raw):
            out.write("\n"+name+"\n"+"-"*len(name)+"\n")
            out.write(raw.decode("utf-8-sig", errors="replace").strip()+"\n")
        for pkg in sorted(packages, key=lambda p:(p["name"], p["version"])):
            if pkg["source"] is None:
                continue
            base = Path(pkg["manifest_path"]).parent
            out.write(f"\n{pkg['name']} {pkg['version']}  |  {pkg['license']}\n{pkg['repository'] or ''}\n")
            found = {p.relative_to(base).as_posix():p.read_bytes() for p in sorted(base.rglob("*"))
                     if p.is_file() and is_notice(p.name) and p.suffix.lower() not in {".rs", ".c", ".h"}}
            if not any("/" not in k for k in found):
                revision = json.loads((base/".cargo_vcs_info.json").read_text())["git"]["sha1"]
                found.update(upstream(pkg["repository"], revision))
            if not found and pkg["license"] == "CC0-1.0":
                with urllib.request.urlopen("https://raw.githubusercontent.com/spdx/license-list-data/v3.26.0/text/CC0-1.0.txt", timeout=25) as response:
                    found["CC0 1.0 Universal"] = response.read()
            if not found:
                raise RuntimeError("Missing license text: "+pkg["name"])
            for name, raw in found.items():
                add(name, raw)
        add("WiX Toolset", opt.wix.read_bytes())
        for filename in ["COPYING3","COPYING.RUNTIME"]:
            add("GCC runtime / "+filename,(opt.rust/"share/doc"/filename).read_bytes())
        for library in ["crt","libgcc","libwinpthread"]:
            for p in sorted((opt.mingw/"share/licenses"/library).glob("*")):
                if p.is_file():
                    add(library+" / "+p.name,p.read_bytes())
        out.write("\nRust standard library credits and licenses are included in Rust-library-notices.html.\n")

if __name__ == "__main__":
    main()
