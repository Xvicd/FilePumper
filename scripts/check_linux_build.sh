#!/usr/bin/env bash
# Проверка Linux-сборки: версия, круговой прогон нагрузки, зависимости.
# Запуск: bash scripts/check_linux_build.sh   (в Linux или WSL)
set -e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Кросс-сборка под x86_64-unknown-linux-gnu имеет отдельный каталог, нативная — target/release.
if [ -d "$ROOT/target/x86_64-unknown-linux-gnu/release" ]; then
  OUT="$ROOT/target/x86_64-unknown-linux-gnu/release"
else
  OUT="$ROOT/target/release"
fi
BIN="$OUT/file-pumper-cli"
GUI="$OUT/file-pumper"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "== версии =="
"$BIN" --version
"$GUI" --version

cd "$WORK"
for i in $(seq 1 40); do echo "carrier line" >> carrier.txt; done
for i in $(seq 1 100); do echo "secret payload line" >> payload.txt; done

echo "== embed =="
"$BIN" embed carrier.txt payload.txt out.bin --copies 2 --compress --password pass123
echo "== inspect =="
"$BIN" inspect out.bin
echo "== extract =="
"$BIN" extract out.bin restored.txt --password pass123
echo "== sha256 =="
sha256sum payload.txt restored.txt
echo "== pump =="
"$BIN" pump carrier.txt pumped.bin --size 1 --method padding
stat -c '%n %s байт' pumped.bin
echo "== ldd =="
ldd "$BIN"
echo "OK"

