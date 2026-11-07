## FilePumper 0.2.0

Накачивает файл до нужного размера и прячет один файл внутри другого — окно приложения и командная строка, Rust.

### Файлы

| Платформа | Архив | Что внутри |
| --- | --- | --- |
| Windows 10/11 x64 | `FilePumper-0.2.0-windows-x64.zip` | `FilePumper.exe` (окно, консоль не открывается), `FilePumper-cli.exe` (терминал), `README.txt` |
| Linux x86_64 | `FilePumper-0.2.0-linux-x64.tar.gz` | `linux/file-pumper` (окно), `linux/file-pumper-cli`, `README.txt` |

### Требования

- **Windows:** DirectX 12 или Vulkan для окна; если система ругается на отсутствующие DLL — Microsoft Visual C++ 2015-2022 x64.
- **Linux:** glibc ≥ 2.17; для окна нужны X11 или Wayland и драйвер Vulkan/Mesa; CLI-версии кроме `libc` ничего не требуется.

### Контрольные суммы (SHA-256)

```
{{WIN_SHA}}  FilePumper-0.2.0-windows-x64.zip
{{LINUX_SHA}}  FilePumper-0.2.0-linux-x64.tar.gz
```

### Как собрано и проверено

- Бинарники собраны в GitHub Actions: Windows — на `windows-latest`, Linux — через `cargo zigbuild` под glibc 2.17.
- Перед сборкой: `cargo test` — 33 теста пройдены, `cargo clippy --all-targets` и `cargo fmt --check` без замечаний.
- Локальный прогон Linux-сборки в WSL: `embed --copies 2 --compress --password` → `inspect` (2 целых копии, deflate, шифрование) → `extract` → SHA-256 совпал; `pump --size 1` дал ровно 1 048 576 байт.
- `FilePumper.exe` — подсистема Windows GUI (консоль не создаётся), `FilePumper-cli.exe` — консольная.

### Быстрый старт (Linux)

```bash
tar -xzf FilePumper-0.2.0-linux-x64.tar.gz
chmod +x linux/file-pumper linux/file-pumper-cli
./linux/file-pumper-cli --help
```

Полное описание режимов, формата контейнера и всех команд — в [README.md](https://github.com/Xvicd/FilePumper/blob/main/README.md).
