#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo "Usage: $0 VERSION TARGET BINARY [OUT_DIR=dist]" >&2
    exit 2
fi
version=$1
target=$2
binary=$3
out_dir=${4:-dist}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ||
      ! $target =~ ^[0-9A-Za-z_]+(-[0-9A-Za-z_]+)+$ ]]; then
    echo "Invalid version or Rust target triple" >&2
    exit 2
fi
if [[ ! -f $binary || ! -x $binary ]]; then
    echo "Binary must be an executable file: $binary" >&2
    exit 2
fi
if [[ $("$binary" --version) != "Beatfall $version" ]]; then
    echo "Binary version does not match $version" >&2
    exit 1
fi
mkdir -p -- "$out_dir"
out_dir=$(cd -- "$out_dir" && pwd)
stem="Beatfall-$version-$target"
archive="$out_dir/$stem.tar.gz"
if [[ -e $archive || -e $archive.sha256 ]]; then
    echo "Refusing to overwrite existing release artifacts: $archive" >&2
    exit 1
fi
tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT
mkdir -- "$tmp/$stem"
cp -- "$binary" "$tmp/$stem/Beatfall"
chmod 755 "$tmp/$stem/Beatfall"
for file in README.md LICENSE CHANGELOG.md; do
    cp -- "$root/$file" "$tmp/$stem/$file"
    chmod 644 "$tmp/$stem/$file"
done
COPYFILE_DISABLE=1 tar -czf "$tmp/$stem.tar.gz" -C "$tmp" "$stem"
if command -v sha256sum >/dev/null 2>&1; then
    (cd -- "$tmp" && sha256sum "$stem.tar.gz") > "$tmp/$stem.tar.gz.sha256"
else
    (cd -- "$tmp" && shasum -a 256 "$stem.tar.gz") > "$tmp/$stem.tar.gz.sha256"
fi
mv -- "$tmp/$stem.tar.gz" "$tmp/$stem.tar.gz.sha256" "$out_dir/"
printf 'Packaged %s\n' "$archive"
