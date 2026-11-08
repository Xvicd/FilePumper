FilePumper 0.5.2 — готовые сборки
===========================================

windows\
  FilePumper.exe      — приложение с окном. Двойной клик, консоль НЕ открывается.
  FilePumper-cli.exe  — версия для командной строки (cmd/PowerShell).

  Команды CLI:
    FilePumper-cli.exe --help
    FilePumper-cli.exe --version
    FilePumper-cli.exe pump <вход>... --size <МБ>[,<МБ>...]
                        [--size-mode exact|at-least|add]
                        [--vary <МБ> [--vary-mode plus|minus|both]]
                        [--names size|plain|random]
                        [--out-dir ПАПКА] [--output ФАЙЛ] [--method ...] [--jobs N]
                        [--dry-run] [--report отчёт.csv|.json] [--skip-unsupported]
    FilePumper-cli.exe pump --from-dir ПАПКА [--recursive] [--ext png,jpg] --size 10
    FilePumper-cli.exe pump --from-file список.txt --size 10
    FilePumper-cli.exe generate --size 10 --type png [--count N] [--name ОСНОВА]
                        [--vary <МБ> [--vary-mode plus|minus|both]]
                        [--names size|plain|random] [--payload <файл> ...] [--out-dir ПАПКА]
                        [--dry-run] [--verify]
    FilePumper-cli.exe embed <носитель>... --payload <файл> [--payload <файл>...]
                        [--out-dir ПАПКА] [--copies 1..3] [--compress] [--password ПАРОЛЬ]
    FilePumper-cli.exe extract <носитель>... [--out-dir ПАПКА] [--password ПАРОЛЬ]
    FilePumper-cli.exe inspect <носитель>...
    FilePumper-cli.exe verify <носитель>...

  Разброс размера: --vary 5 делает размер случайным в пределах ±5 МБ
  (--size 400 --vary 5 → файл на 395…405 МБ). Направление задаёт
  --vary-mode: both — в обе стороны (по умолчанию), plus — только прибавлять,
  minus — только убавлять. Отклонение считается для каждого файла отдельно, а
  файл записывается ровно того размера, который выпал (--verify это проверяет).
  Пример: .\FilePumper-cli.exe generate --size 400 --vary 5 --type png --out-dir .\big

  Файлы с нуля: generate (алиас make) создаёт файл нужного размера без исходника —
  внутри криптослучайные байты, поэтому zip/gzip/7z его не сожмут. Тип задаётся
  --type bin|png|gif|jpg|webp: картиночные типы остаются открывающимися файлами.
  С --payload внутрь прячется нагрузка, но размер остаётся ровно целевым.
  Пример: .\FilePumper-cli.exe generate --size 5,10 --count 2 --type png --out-dir .\big

  Имена результатов: --names size (по умолчанию, размер в имени), --names plain
  (photo.png), --names random (quiet-river-4821.png). Занятое имя получает суффикс.

  Пакетная накачка: можно передать сразу несколько файлов. Ошибка одного файла
  не прерывает остальные, в конце печатается сводка. Результаты получают имя
  <имя>_<размер>MB.<расширение> и не перезаписывают существующие файлы.
  --dry-run печатает план и ничего не создаёт, --report сохраняет отчёт
  по каждому файлу, --jobs N обрабатывает файлы параллельно.
  Пример (PowerShell):
    $files = Get-ChildItem .\photos\*.jpg | ForEach-Object FullName
    .\FilePumper-cli.exe pump @files --size 10 --method png-chunks --out-dir .\big --jobs 4

  Нагрузка: повторный --payload упаковывает несколько файлов в ZIP-бандл внутри
  носителя. extract распаковывает бандл по файлам, inspect показывает оглавление,
  verify перечитывает носитель и проверяет SHA-256 без записи на диск.

  Если запустить FilePumper.exe с аргументами, команда выполнится,
  но вывода в консоли не будет (её нет) — ошибки показываются диалогом.
  Для терминала используйте FilePumper-cli.exe.

  Требования: Windows 10/11 x64. Для окна нужен драйвер DirectX 12 или Vulkan
  (на современных системах уже есть). Если система пожалуется на отсутствие
  библиотек Visual C++, установите "Microsoft Visual C++ Redistributable
  2015-2022 x64".

linux\
  file-pumper      — приложение с окном (запускать из терминала: ./file-pumper).
  file-pumper-cli  — только команды CLI, самый маленький.

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
для устойчивости к обрезанию файла сервером, ZIP-бандл для нескольких файлов
(`embed ... --payload a.txt --payload b.txt` — 0.4.0 и новее).
