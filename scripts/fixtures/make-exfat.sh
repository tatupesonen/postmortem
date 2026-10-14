#!/usr/bin/env bash
# Regenerates fixtures/exfat/basic.raw.gz using macOS's native exFAT driver.
# Requires macOS (hdiutil). The image is mounted privately (-nobrowse).
set -euo pipefail
out="$(cd "$(dirname "$0")/../.." && pwd)/fixtures/exfat/basic.raw.gz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
hdiutil create -size 8m -fs ExFAT -volname PMEXFAT -layout NONE -type UDIF ex.dmg >/dev/null
mp="$(hdiutil attach -nobrowse -mountrandom "$work" ex.dmg | awk '{print $NF}' | tail -1)"
(
  cd "$mp"
  mkdir -p "DCIM/100CANON" docs
  python3 - <<'PY'
open('hello.txt','w').write('hello exfat\n')
open('A very long file name with unicode ääkköset and spaces.txt','w').write('long name\n')
open('docs/report.txt','w').write('report '*2000)
open('DCIM/100CANON/IMG_0001.bin','wb').write(bytes(i%251 for i in range(300000)))
open('deleted.txt','w').write('soon deleted content\n')
open('docs/also_deleted.bin','wb').write(b'X'*50000)
PY
  sync
  rm deleted.txt docs/also_deleted.bin
  sync
)
hdiutil detach "$mp" >/dev/null
hdiutil convert ex.dmg -format UDTO -o ex.raw >/dev/null
gzip -9 -c ex.raw.cdr > "$out"
echo "wrote $out"
