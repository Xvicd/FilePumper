FilePumper 0.6.3 — готовые сборки
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
                        [--names size|plain|random] [--random-style words|chars]
                        [--random-length N|N-M|random] [--random-alphabet НАБОР]
                        [--out-dir ПАПКА] [--output ФАЙЛ] [--method ...] [--jobs N]
                        [--dry-run] [--report отчёт.csv|.json] [--skip-unsupported]
    FilePumper-cli.exe pump --from-dir ПАПКА [--recursive] [--ext png,jpg] --size 10
    FilePumper-cli.exe pump --from-file список.txt --size 10
    FilePumper-cli.exe generate --size 10 --type png[,gif,...] [--mix even|random]
                        [--count N] [--image-size WxH] [--noise rgb|gradient|checker|tv|perlin]
                        [--name ОСНОВА]
                        [--vary <МБ> [--vary-mode plus|minus|both]]
                        [--names size|plain|random] [--random-style words|chars]
                        [--payload <файл> ...] [--out-dir ПАПКА]
                        [--dry-run] [--verify]
    FilePumper-cli.exe embed <носитель>... --payload <файл> [--payload <файл>...]
                        [--out-dir ПАПКА] [--copies 1..3] [--compress] [--password ПАРОЛЬ]
                        [--pair [--pair-password ПАРОЛЬ ...]]
    FilePumper-cli.exe extract <носитель>... [--out-dir ПАПКА] [--password ПАРОЛЬ]
    FilePumper-cli.exe inspect <носитель>... [--json]
    FilePumper-cli.exe verify <носитель>... [--json]
    FilePumper-cli.exe share <файл> --parts N --threshold K [--out-dir ПАПКА]
    FilePumper-cli.exe join <часть> <часть> ... [--output ФАЙЛ]
    FilePumper-cli.exe clean <файл>... [--out-dir ПАПКА] [--output ФАЙЛ]
                        [--suffix _clean] [--profiles] [--dry-run] [--json]

  Для скриптов: --quiet (без прогресса и служебного текста), --no-progress,
  --json (машинный вывод), --password-file ФАЙЛ, --password-stdin и --jobs 0
  (число потоков по числу ядер). Пары можно сопоставлять по именам:
  --pair-by name (photo.png ← photo.zip, report.pdf ← report.mp3).

  Схема Шамира: share делит файл на части (любые --threshold из --parts
  восстанавливают исходник), join собирает обратно. Части — обычные файлы
  <имя>.share1, <имя>.share2, ...

  Ключ-файл: --keyfile ФАЙЛ подмешивается к паролю при выводе ключа
  (embed/extract/verify). Проверка перекодирования: inspect --recompress.

  Метод --method office работает и с медиа: MP3 получает padding внутри
  ID3v2-тега, FLAC — блок PADDING, MP4/MOV — free-бокс.

  Режим размера --size-mode percent прибавляет N процентов к исходному файлу:
  --size 50 --size-mode percent.

  Разброс размера: --vary 5 делает размер случайным в пределах ±5 МБ
  (--size 400 --vary 5 → файл на 395…405 МБ). Направление задаёт
  --vary-mode: both — в обе стороны (по умолчанию), plus — только прибавлять,
  minus — только убавлять. Отклонение считается для каждого файла отдельно, а
  файл записывается ровно того размера, который выпал (--verify это проверяет).
  Пример: .\FilePumper-cli.exe generate --size 400 --vary 5 --type png --out-dir .\big

  Файлы с нуля: generate (алиас make) создаёт файл нужного размера без исходника —
  внутри криптослучайные байты, поэтому zip/gzip/7z его не сожмут. Тип задаётся
  --type bin|png|gif|jpg|webp|pdf|docx|xlsx: картиночные типы остаются
  открывающимися файлами, pdf/docx/xlsx — настоящие документы ровно нужного
  размера.
  Типов может быть несколько: --type png,gif,bin (список через запятую, all — все
  пять). Число файлов задаёт --count (от 1 до 10000) или поле в окне.
  Размер видимой картинки задаёт --image-size 512x512 (по умолчанию 16x16):
  файл выглядит как шумовое фото нужного разрешения. Вид шума выбирает --noise:
  rgb (цветной снег), gradient, checker, tv, perlin.
  Размер можно писать с единицами: --size 700KB, --size 1.5GB, --size 512B.
  Как разложить типы, решает --mix: even (по умолчанию) — по --count файлов
  каждого типа, random — всего --count файлов со случайным типом.
  С --payload внутрь прячется нагрузка, но размер остаётся ровно целевым.
  Пример: .\FilePumper-cli.exe generate --size 5,10 --count 2 --type png --out-dir .\big
  Пример: .\FilePumper-cli.exe generate --size 10 --type png,gif --count 3 --out-dir .\big
  Пример: .\FilePumper-cli.exe generate --size 10 --type bin,png,gif --count 6 --mix random --out-dir .\big

  Имена результатов: --names size (по умолчанию, размер в имени), --names plain
  (photo.png), --names random (green-apple-4821.png). Дробный размер пишется
  через «_»: photo_12_3MB.png (точка выглядела бы началом расширения).
  Занятое имя получает суффикс -2, -3, …
  Вид случайного имени: --random-style words|chars (слова или набор символов),
  --random-length N|N-M|random (3…48; «5-12» — своя длина у каждого имени,
  random — 4…16) и --random-alphabet НАБОР (например
  0123456789abcdef; годятся латиница, цифры, «-» и «_»). Пример:
  .\FilePumper-cli.exe pump .\photos\* --size 5 --names random --random-style chars --random-length 8

  Пакетная накачка: можно передать сразу несколько файлов. Ошибка одного файла
  не прерывает остальные, в конце печатается сводка. Результаты получают имя
  <имя>_<размер>MB.<расширение> и не перезаписывают существующие файлы.
  --dry-run печатает план и ничего не создаёт, --report сохраняет отчёт
  по каждому файлу, --jobs N обрабатывает файлы параллельно.
  Пример (PowerShell):
    $files = Get-ChildItem .\photos\*.jpg | ForEach-Object FullName
    .\FilePumper-cli.exe pump @files --size 10 --method png-chunks --out-dir .\big --jobs 4

  Нагрузка: повторный --payload упаковывает несколько файлов в ZIP-бандл внутри
  носителя. Для docx/xlsx/pptx нагрузка — отдельная часть пакета, для PDF —
  неиспользуемый поток-объект. extract --only ИМЯ достаёт из бандла одну запись.
  extract распаковывает бандл по файлам, inspect показывает оглавление,
  verify перечитывает носитель и проверяет SHA-256 без записи на диск.

  Метаданные: clean убирает автора, даты, программы и комментарии из PNG, JPEG,
  GIF, WebP, SVG, docx/xlsx/pptx и PDF. Пиксели и текст документа не меняются,
  рядом появляется копия с суффиксом _clean (--suffix меняет суффикс, --profiles
  добавляет ICC-профили и превью документов). --dry-run показывает, что найдено.
  Пример: .\FilePumper-cli.exe clean .\photo.jpg .\report.docx --out-dir .\clean

  Группы файлов: embed --pair раскладывает нагрузки по носителям по порядку
  (--pair-password ПАРОЛЬ задаёт пароль пары, флаг можно повторять). inspect с
  несколькими путями печатает, что где спрятано, и итог «что можно извлечь»;
  extract с несколькими носителями достаёт всё найденное в одну папку, а обычные
  файлы просто помечает «нагрузки нет» (код выхода 0). Извлечённый файл сохраняет
  расширение из имени нагрузки: song.mp3, а не безымянный файл.

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
WebP (чанк UFP1), SVG (metadata), DOCX/XLSX/PPTX (часть пакета) и PDF
(неиспользуемый поток) или в хвост любого другого файла.
Опции: сжатие deflate, пароль (Argon2id + ChaCha20-Poly1305), 1-3 копии
для устойчивости к обрезанию файла сервером, ZIP-бандл для нескольких файлов
(`embed ... --payload a.txt --payload b.txt` — 0.4.0 и новее).
