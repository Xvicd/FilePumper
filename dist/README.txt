FilePumper 0.2.0 — готовые сборки
===========================================

windows\
  FilePumper.exe      — приложение с окном. Двойной клик, консоль НЕ открывается.
  FilePumper-cli.exe  — версия для командной строки (cmd/PowerShell).

  Команды CLI:
    FilePumper-cli.exe --help
    FilePumper-cli.exe --version
    FilePumper-cli.exe pump <вход> <выход> --size <МБ> [--method ...]
    FilePumper-cli.exe embed <носитель> <нагрузка> [выход] [--copies 1..3] [--compress] [--password ПАРОЛЬ]
    FilePumper-cli.exe extract <носитель> [выход] [--password ПАРОЛЬ]
    FilePumper-cli.exe inspect <носитель>

  Если запустить FilePumper.exe с аргументами, команда выполнится,
  но вывода в консоли не будет (её нет) — ошибки показываются диалогом.
  Для терминала используйте FilePumper-cli.exe.

  Требования: Windows 10/11 x64. Для окна нужен драйвер DirectX 12 или Vulkan
  (на современных системах уже есть). Если система пожалуется на отсутствие
  библиотек Visual C++, установите "Microsoft Visual C++ Redistributable
  2015-2022 x64".

linux\
  file-pumper      — приложение с окном (запускать из терминала: ./file-pumper).
  file-pumper-cli  — только команды CLI, самый маленький (2.9 МБ).

  Установка:
    chmod +x file-pumper file-pumper-cli
    ./file-pumper-cli --version

  Требования к CLI-версии: любой Linux x86_64 с glibc 2.17+ (проверено на glibc 2.43),
  кроме libc ничего не нужно.

  Требования к окну: X11 или Wayland, драйвер Vulkan (mesa-vulkan-drivers /
  vulkan-radeon / nvidia) либо программный lavapipe; для wayland — libwayland.
  На Debian/Ubuntu обычно достаточно:
    sudo apt install libgl1-mesa-dri mesa-vulkan-drivers libxkbcommon0 libwayland-client0

  Без графической сессии окно не запустится — используйте CLI-версию.

Данные внутри файла
-------------------
Нагрузка встраивается в PNG (приватные чанки), JPEG (APP15), GIF (comment),
WebP (чанк UFP1), SVG (metadata) или в хвост любого другого файла.
Опции: сжатие deflate, пароль (Argon2id + ChaCha20-Poly1305), 1-3 копии
для устойчивости к обрезанию файла сервером.
