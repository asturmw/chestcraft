#!/bin/sh
# Copies the files the binary ships from the design branch into assets/: tokens.css,
# app.css, icons.svg, skins/, fonts/ and the backgrounds (docs/ui-handoff.md, section 5).
# They stay unchanged; the design session owns them. Usage: deploy/design-sync.sh [REF]
# (default: the local branch `design`). Prints what changed; review it with `git diff`.
set -eu
cd "$(dirname "$0")/.."
ref=${1:-design}
git rev-parse --verify --quiet "$ref^{commit}" >/dev/null || {
    echo "no such ref: $ref" >&2
    exit 1
}

copy() { # design/<path> -> assets/<path>
    src=$1
    dst=assets/${src#design/}
    mkdir -p "$(dirname "$dst")"
    tmp=$dst.sync-tmp
    git show "$ref:$src" >"$tmp"
    if [ -f "$dst" ] && cmp -s "$tmp" "$dst"; then
        rm -f "$tmp"
    else
        mv "$tmp" "$dst"
        echo "updated $dst"
    fi
}

for f in design/tokens.css design/app.css design/icons.svg; do
    copy "$f"
done
git ls-tree -r --name-only "$ref" -- design/skins design/fonts design/backgrounds |
    while read -r f; do
        case $f in
        *.css | *.woff2 | *.txt | *.svg | *.jpg) copy "$f" ;;
        esac
    done

# Files the design branch no longer has.
for dir in skins fonts; do
    [ -d "assets/$dir" ] || continue
    for f in assets/$dir/*; do
        git cat-file -e "$ref:design/${f#assets/}" 2>/dev/null || echo "gone on $ref (remove by hand): $f"
    done
done
