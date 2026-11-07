#!/usr/bin/env bash
# Проверка Linux-сборки: версия, круговой прогон нагрузки, зависимости.
set -e
ROOT="/mnt/d/python proj"
BIN="$ROOT/target/x86_64-unknown-linux-gnu/release/file-pumper-cli"
GUI="$ROOT/target/x86_64-unknown-linux-gnu/release/file-pumper"

echo "== версии =="
"$BIN" --version
"$GUI" --version

WORK=/tmp/ufptest
rm -rf "$WORK"
mkdir -p "$WORK"
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
