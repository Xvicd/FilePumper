//! Графический интерфейс: накачка файлов, встраивание и извлечение нагрузки.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::batch::{
    self, BatchItem, GenerateTargets, JitterMode, NameMode, PumpPlan, SizeJitter, TargetMode,
};
use crate::payload::{self, ContainerOptions, PayloadSummary};
use crate::pumper::{AnimationFormat, GeneratedKind, Method};
use crate::scan;
use crate::util::{reveal_in_explorer, CancelToken, Job, Reservations, CANCELLED_MESSAGE, MIB};

/// Цвет успешного результата в списке файлов.
const OK_COLOR: egui::Color32 = egui::Color32::from_rgb(0x3c, 0xb3, 0x71);
/// Цвет предупреждения: разброс, лимиты метода.
const WARN_COLOR: egui::Color32 = egui::Color32::from_rgb(0xd9, 0x8a, 0x2f);
/// Цвет ошибки.
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xd9, 0x53, 0x4f);

enum WorkerMessage {
    Progress(f32, String),
    Finished(Result<(String, PathBuf), String>),
    Inspected(Result<PayloadSummary, String>),
    BatchItemStarted(usize),
    BatchItemDone(usize, Result<u64, String>),
    BatchFinished {
        ok: bool,
        cancelled: bool,
        text: String,
        folder: Option<PathBuf>,
    },
}

/// Строка списка пакета: файл и его состояние.
struct BatchRow {
    input: PathBuf,
    status: String,
    ok: Option<bool>,
}

struct Preview {
    summary: PayloadSummary,
    password: String,
    error: Option<String>,
}

/// Вкладка окна: три задачи программы, каждая со своими настройками.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    /// Накачать выбранные файлы до нужного размера.
    #[default]
    Pump,
    /// Спрятать нагрузку внутрь картинки и достать её обратно.
    Payload,
    /// Создать файл нужного размера без исходника.
    Generate,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Pump, Tab::Payload, Tab::Generate];

    fn title(self) -> &'static str {
        match self {
            Tab::Pump => "Накачать файлы",
            Tab::Payload => "Спрятать / извлечь",
            Tab::Generate => "Создать файл",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Tab::Pump => "Добавьте файлы, задайте размер и метод — получите файлы нужного объёма, которые продолжают открываться.",
            Tab::Payload => "Спрячьте любой файл внутрь картинки и достаньте его обратно: данные лежат в структуре файла, картинка остаётся рабочей.",
            Tab::Generate => "Файл нужного размера без исходника: внутри криптослучайные байты, поэтому zip, gzip и 7z его не сожмут.",
        }
    }

    fn index(self) -> usize {
        Tab::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn from_index(index: usize) -> Self {
        Tab::ALL.get(index).copied().unwrap_or_default()
    }
}

/// Настройки, которые переживают перезапуск. Пароль намеренно не сохраняется.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    method: usize,
    format: usize,
    target_mb: String,
    vary_mb: String,
    vary_mode: usize,
    size_mode: usize,
    out_dir: Option<PathBuf>,
    embed_compress: bool,
    embed_copies: usize,
    names: usize,
    tab: usize,
    gen_kind: usize,
    gen_base: String,
    gen_count: usize,
    gen_attach: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            method: 0,
            format: 0,
            target_mb: "10".to_owned(),
            vary_mb: "0".to_owned(),
            vary_mode: JitterMode::ALL
                .iter()
                .position(|mode| *mode == JitterMode::default())
                .unwrap_or(0),
            size_mode: 0,
            out_dir: None,
            embed_compress: false,
            embed_copies: 1,
            names: 0,
            tab: 0,
            gen_kind: 0,
            gen_base: "file".to_owned(),
            gen_count: 1,
            gen_attach: false,
        }
    }
}

fn method_from_index(index: usize) -> Method {
    Method::ALL.get(index).copied().unwrap_or_default()
}

fn method_index(method: Method) -> usize {
    Method::ALL
        .iter()
        .position(|item| *item == method)
        .unwrap_or(0)
}

fn format_from_index(index: usize) -> AnimationFormat {
    match index {
        1 => AnimationFormat::Png,
        2 => AnimationFormat::Webp,
        _ => AnimationFormat::Gif,
    }
}

fn format_index(format: AnimationFormat) -> usize {
    match format {
        AnimationFormat::Gif => 0,
        AnimationFormat::Png => 1,
        AnimationFormat::Webp => 2,
    }
}

fn generated_kind_from_index(index: usize) -> GeneratedKind {
    GeneratedKind::ALL.get(index).copied().unwrap_or_default()
}

fn generated_kind_index(kind: GeneratedKind) -> usize {
    GeneratedKind::ALL
        .iter()
        .position(|item| *item == kind)
        .unwrap_or(0)
}

pub struct PumperApp {
    rows: Vec<BatchRow>,
    out_dir: Option<PathBuf>,
    method: Method,
    animation_format: AnimationFormat,
    target_mb: String,
    size_mode: TargetMode,
    vary_mb: String,
    vary_mode: JitterMode,
    tab: Tab,
    status: String,
    progress: f32,
    worker: Option<Receiver<WorkerMessage>>,
    cancel: CancelToken,
    busy: bool,
    result_dialog: Option<(bool, String, Option<PathBuf>)>,
    carriers: Vec<PathBuf>,
    payloads: Vec<PathBuf>,
    embed_compress: bool,
    embed_password: String,
    embed_copies: usize,
    names: NameMode,
    gen_kind: GeneratedKind,
    gen_base: String,
    gen_count: usize,
    gen_attach: bool,
    preview: Option<Preview>,
}

impl Default for PumperApp {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            out_dir: None,
            method: Method::default(),
            animation_format: AnimationFormat::default(),
            target_mb: "10".to_owned(),
            size_mode: TargetMode::default(),
            vary_mb: "0".to_owned(),
            vary_mode: JitterMode::default(),
            tab: Tab::default(),
            status: "Добавьте файлы и режим, затем нажмите «Запустить».".to_owned(),
            progress: 0.0,
            worker: None,
            cancel: CancelToken::new(),
            busy: false,
            result_dialog: None,
            carriers: Vec::new(),
            payloads: Vec::new(),
            embed_compress: false,
            embed_password: String::new(),
            embed_copies: 1,
            names: NameMode::Size,
            gen_kind: GeneratedKind::default(),
            gen_base: "file".to_owned(),
            gen_count: 1,
            gen_attach: false,
            preview: None,
        }
    }
}

fn channel() -> (mpsc::Sender<WorkerMessage>, Receiver<WorkerMessage>) {
    mpsc::channel()
}

impl PumperApp {
    /// Создаёт приложение и восстанавливает настройки прошлого запуска.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::default();
        if let Some(storage) = cc.storage {
            if let Some(settings) = eframe::get_value::<Settings>(storage, eframe::APP_KEY) {
                app.apply_settings(settings);
            }
        }
        app
    }

    fn settings(&self) -> Settings {
        Settings {
            method: method_index(self.method),
            format: format_index(self.animation_format),
            target_mb: self.target_mb.clone(),
            vary_mb: self.vary_mb.clone(),
            vary_mode: JitterMode::ALL
                .iter()
                .position(|mode| *mode == self.vary_mode)
                .unwrap_or(0),
            size_mode: TargetMode::ALL
                .iter()
                .position(|mode| *mode == self.size_mode)
                .unwrap_or(0),
            out_dir: self.out_dir.clone(),
            embed_compress: self.embed_compress,
            embed_copies: self.embed_copies,
            names: NameMode::ALL
                .iter()
                .position(|mode| *mode == self.names)
                .unwrap_or(0),
            tab: self.tab.index(),
            gen_kind: generated_kind_index(self.gen_kind),
            gen_base: self.gen_base.clone(),
            gen_count: self.gen_count,
            gen_attach: self.gen_attach,
        }
    }

    fn apply_settings(&mut self, settings: Settings) {
        self.method = method_from_index(settings.method);
        self.animation_format = format_from_index(settings.format);
        if !settings.target_mb.trim().is_empty() {
            self.target_mb = settings.target_mb;
        }
        self.vary_mb = if settings.vary_mb.trim().is_empty() {
            "0".to_owned()
        } else {
            settings.vary_mb
        };
        self.vary_mode = JitterMode::ALL
            .get(settings.vary_mode)
            .copied()
            .unwrap_or_default();
        self.size_mode = TargetMode::ALL
            .get(settings.size_mode)
            .copied()
            .unwrap_or_default();
        self.out_dir = settings.out_dir;
        self.embed_compress = settings.embed_compress;
        self.embed_copies = settings.embed_copies.clamp(1, 3);
        self.names = NameMode::ALL
            .get(settings.names)
            .copied()
            .unwrap_or_default();
        self.tab = Tab::from_index(settings.tab);
        self.gen_kind = generated_kind_from_index(settings.gen_kind);
        if !settings.gen_base.trim().is_empty() {
            self.gen_base = settings.gen_base;
        }
        self.gen_count = settings.gen_count.clamp(1, 64);
        self.gen_attach = settings.gen_attach;
    }

    /// Как называть результаты: с размером в имени, без него или случайно.
    fn name_mode(&self) -> NameMode {
        self.names
    }

    /// Пример имени результата для текущих настроек (показывается под кнопками).
    fn example_name(&self) -> String {
        let mb = self
            .target_mb
            .trim()
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(10.0);
        batch::result_name(
            Path::new("photo.png"),
            (mb * MIB as f64) as u64,
            self.method,
            self.animation_format,
            self.name_mode(),
        )
    }

    /// Размер из поля «Размер» в мегабайтах (запятая тоже работает).
    fn target_mb_value(&self) -> Option<f64> {
        self.target_mb
            .trim()
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0)
    }

    /// Разброс размера из полей окна: `Ok(None)` — разброс выключен.
    fn jitter(&self) -> Result<Option<SizeJitter>, String> {
        let raw = self.vary_mb.trim().replace(',', ".");
        if raw.is_empty() {
            return Ok(None);
        }
        let spread = raw
            .parse::<f64>()
            .map_err(|_| "Разброс должен быть числом в мегабайтах.".to_owned())?;
        SizeJitter::from_mb(spread, self.vary_mode)
    }

    /// Подсказка «какой размер получится» для текущих настроек разброса.
    fn size_range_hint(&self) -> Option<String> {
        let mb = self.target_mb_value()?;
        let jitter = self.jitter().ok().flatten()?;
        let base = (mb * MIB as f64) as u64;
        let (low, high) = jitter.range(base);
        Some(format!(
            "Каждый файл получит случайный размер от {:.2} до {:.2} МБ ({}).",
            low as f64 / MIB as f64,
            high as f64 / MIB as f64,
            jitter.label()
        ))
    }

    /// Пример имени файла, созданного «с нуля» (показывается под кнопкой).
    fn generated_example_name(&self) -> String {
        let target = (self.target_mb_value().unwrap_or(10.0) * MIB as f64) as u64;
        match self.name_mode() {
            // Случайное имя менялось бы каждый кадр, поэтому показываем образец.
            NameMode::Random => format!("quiet-river-4821.{}", self.gen_kind.extension()),
            names => batch::generated_name(&self.gen_base, target, self.gen_kind, names),
        }
    }

    /// Имя результата встраивания: `<носитель>_with_payload.<ext>`, а при
    /// включённых случайных именах — случайная основа с тем же расширением.
    fn embed_suggested_path(&self, carrier: &Path) -> PathBuf {
        let suggested = payload::default_output_path(carrier);
        if self.names == NameMode::Random {
            batch::with_random_stem(&suggested)
        } else {
            suggested
        }
    }

    /// Носитель для предпросмотра и извлечения — первый в списке.
    fn primary_carrier(&self) -> Option<PathBuf> {
        self.carriers.first().cloned()
    }

    fn begin_job(&mut self) -> (mpsc::Sender<WorkerMessage>, CancelToken) {
        let (sender, receiver) = channel();
        self.cancel = CancelToken::new();
        self.worker = Some(receiver);
        self.busy = true;
        self.progress = 0.0;
        (sender, self.cancel.clone())
    }

    /// Добавляет файлы в список пакета, пропуская повторы и не-файлы.
    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let mut added = 0_usize;
        for path in paths {
            if !path.is_file() {
                continue;
            }
            if self
                .rows
                .iter()
                .any(|row| crate::util::same_file(&row.input, &path))
            {
                continue;
            }
            self.rows.push(BatchRow {
                input: path,
                status: "Ожидает".to_owned(),
                ok: None,
            });
            added += 1;
        }
        if added > 0 {
            self.status = format!("Файлов в списке: {}.", self.rows.len());
        }
    }

    fn start_pump(&mut self) {
        if self.rows.is_empty() {
            self.result_dialog = Some((false, "Добавьте хотя бы один файл.".to_owned(), None));
            return;
        }
        let normalized = self.target_mb.trim().replace(',', ".");
        let mb = match normalized.parse::<f64>() {
            Ok(value) if value.is_finite() && value > 0.0 => value,
            _ => {
                self.result_dialog =
                    Some((false, "Введите корректный размер в МБ.".to_owned(), None));
                return;
            }
        };
        let method = self.method;
        let format = self.animation_format;
        let out_dir = self.out_dir.clone();
        let jitter = match self.jitter() {
            Ok(jitter) => jitter,
            Err(message) => {
                self.result_dialog = Some((false, message, None));
                return;
            }
        };
        let plan = match PumpPlan::new(method, format, out_dir.clone(), self.size_mode, &[mb]) {
            Ok(plan) => plan.with_names(self.name_mode()).with_jitter(jitter),
            Err(message) => {
                self.result_dialog = Some((false, message, None));
                return;
            }
        };
        let inputs: Vec<PathBuf> = self.rows.iter().map(|row| row.input.clone()).collect();
        let planned = plan.build(&inputs);

        for row in self.rows.iter_mut() {
            row.ok = None;
            row.status = "Ожидает".to_owned();
        }
        for (input, message) in &planned.rejected {
            if let Some(row) = self.rows.iter_mut().find(|row| &row.input == input) {
                row.status = format!("Пропуск: {message}");
                row.ok = Some(false);
            }
        }
        let mut work: Vec<(usize, BatchItem)> = Vec::new();
        for item in planned.items {
            if let Some(index) = self.rows.iter().position(|row| row.input == item.input) {
                work.push((index, item));
            }
        }
        if work.is_empty() {
            self.result_dialog = Some((false, "Ни один файл не подошёл.".to_owned(), None));
            return;
        }

        let (sender, cancel) = self.begin_job();
        self.status = "Подготовка...".to_owned();
        thread::spawn(move || {
            let total = work.len();
            let mut created = 0_usize;
            let mut failed = 0_usize;
            let mut cancelled = false;
            let mut folder: Option<PathBuf> = None;
            // Общий контекст нужен только ради общего флага отмены: прогресс
            // каждого файла пересчитывается в общий дочерним контекстом.
            let silent = |_: f32, _: &str| {};
            let job = Job::new(&silent, &cancel);

            for (position, (row_index, item)) in work.into_iter().enumerate() {
                if cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let _ = sender.send(WorkerMessage::BatchItemStarted(row_index));
                let progress_sender = sender.clone();
                let progress = move |percent: f32, text: &str| {
                    let within = percent.clamp(0.0, 100.0) / 100.0;
                    let overall = ((position as f32 + within) / total as f32).clamp(0.0, 1.0);
                    let _ = progress_sender.send(WorkerMessage::Progress(
                        overall,
                        format!("Файл {}/{}: {text}", position + 1, total),
                    ));
                };
                let item_job = job.child(&progress);
                match batch::pump_one(&item, method, format, item.target, &item_job) {
                    Ok(size) => {
                        created += 1;
                        folder = item.output.parent().map(PathBuf::from);
                        let _ = sender.send(WorkerMessage::BatchItemDone(row_index, Ok(size)));
                    }
                    Err(message) if message == CANCELLED_MESSAGE => {
                        cancelled = true;
                        let _ = sender.send(WorkerMessage::BatchItemDone(row_index, Err(message)));
                        break;
                    }
                    Err(message) => {
                        failed += 1;
                        let _ = sender.send(WorkerMessage::BatchItemDone(row_index, Err(message)));
                    }
                }
            }

            let mut text = if cancelled {
                format!("Обработка отменена.\nГотово файлов: {created} из {total}.")
            } else {
                format!("Готово файлов: {created} из {total}.")
            };
            if failed > 0 {
                text.push_str(&format!("\nС ошибкой: {failed}."));
            }
            match folder.as_ref() {
                Some(directory) => {
                    text.push_str(&format!("\nПапка результатов: {}", directory.display()));
                }
                None => text.push_str("\nРезультаты лежат рядом с исходными файлами."),
            }
            let _ = sender.send(WorkerMessage::BatchFinished {
                ok: !cancelled && failed == 0,
                cancelled,
                text,
                folder,
            });
        });
    }

    /// Создаёт файл «с нуля»: размер из пункта 3, тип и основа имени — из пункта 5.
    /// Если включено «Прицепить второе дно», внутрь уходит нагрузка из пункта 4.
    fn start_generate(&mut self) {
        let Some(mb) = self.target_mb_value() else {
            self.result_dialog = Some((false, "Введите корректный размер в МБ.".to_owned(), None));
            return;
        };
        if self.gen_attach && self.payloads.is_empty() {
            self.result_dialog = Some((
                false,
                "Для второго дна выберите файлы нагрузки в пункте 4.".to_owned(),
                None,
            ));
            return;
        }
        if self.gen_attach {
            if let Some(bad) = self.payloads.iter().find(|path| !path.is_file()) {
                self.result_dialog =
                    Some((false, format!("Файл не найден: {}", bad.display()), None));
                return;
            }
        }
        if let Some(directory) = self.out_dir.as_ref() {
            if !directory.is_dir() {
                if let Err(err) = std::fs::create_dir_all(directory) {
                    self.result_dialog =
                        Some((false, format!("Не удалось создать папку: {err}"), None));
                    return;
                }
            }
        }
        let targets = match batch::targets_from_mb(&[mb]) {
            Ok(targets) => targets,
            Err(message) => {
                self.result_dialog = Some((false, message, None));
                return;
            }
        };
        let jitter = match self.jitter() {
            Ok(jitter) => jitter,
            Err(message) => {
                self.result_dialog = Some((false, message, None));
                return;
            }
        };
        let payloads = if self.gen_attach {
            self.payloads.clone()
        } else {
            Vec::new()
        };
        let spec = GenerateTargets {
            sizes: targets,
            count: self.gen_count,
            jitter,
        };
        let items = match batch::plan_generate(
            &self.gen_base,
            self.gen_kind,
            self.name_mode(),
            self.out_dir.as_deref(),
            &spec,
            &payloads,
        ) {
            Ok(items) => items,
            Err(message) => {
                self.result_dialog = Some((false, message, None));
                return;
            }
        };
        if items.is_empty() {
            self.result_dialog = Some((false, "Нечего создавать.".to_owned(), None));
            return;
        }

        let options = ContainerOptions {
            compress: self.embed_compress,
            password: password_or_none(&self.embed_password),
            bundle: false,
        };
        let copies = self.embed_copies.clamp(1, 3);
        let (sender, cancel) = self.begin_job();
        self.status = "Создание файлов...".to_owned();
        thread::spawn(move || {
            let total = items.len();
            let mut created = 0_usize;
            let mut failed = 0_usize;
            let mut cancelled = false;
            let mut last_error: Option<String> = None;
            let mut folder: Option<PathBuf> = None;
            // Общий контекст нужен только ради флага отмены: прогресс каждого
            // файла пересчитывается в общий через дочерний контекст.
            let silent = |_: f32, _: &str| {};
            let job = Job::new(&silent, &cancel);

            for (position, item) in items.iter().enumerate() {
                if cancel.is_cancelled() {
                    cancelled = true;
                    break;
                }
                let progress_sender = sender.clone();
                let progress = move |percent: f32, text: &str| {
                    let within = percent.clamp(0.0, 100.0) / 100.0;
                    let overall = ((position as f32 + within) / total as f32).clamp(0.0, 1.0);
                    let _ = progress_sender.send(WorkerMessage::Progress(
                        overall,
                        format!("Файл {}/{}: {text}", position + 1, total),
                    ));
                };
                let item_job = job.child(&progress);
                match batch::generate_one(item, &options, copies, &item_job) {
                    Ok(_) => {
                        created += 1;
                        folder = item.output.parent().map(PathBuf::from);
                    }
                    Err(message) if message == CANCELLED_MESSAGE => {
                        cancelled = true;
                        break;
                    }
                    Err(message) => {
                        failed += 1;
                        last_error = Some(message);
                    }
                }
            }

            let mut text = if cancelled {
                format!("Создание отменено.\nГотово файлов: {created} из {total}.")
            } else {
                format!("Готово файлов: {created} из {total}.")
            };
            if failed > 0 {
                text.push_str(&format!("\nС ошибкой: {failed}."));
            }
            if let Some(message) = last_error {
                text.push_str(&format!("\nПоследняя ошибка: {message}"));
            }
            match folder.as_ref() {
                Some(directory) => {
                    text.push_str(&format!("\nПапка результатов: {}", directory.display()));
                }
                None => text.push_str("\nФайлы созданы в рабочей папке."),
            }
            let _ = sender.send(WorkerMessage::BatchFinished {
                ok: !cancelled && failed == 0,
                cancelled,
                text,
                folder,
            });
        });
    }

    fn start_inspect(&mut self) {
        let Some(carrier) = self.primary_carrier() else {
            self.result_dialog = Some((false, "Выберите файл-носитель.".to_owned(), None));
            return;
        };
        if !carrier.is_file() {
            self.result_dialog = Some((false, "Файл-носитель не найден.".to_owned(), None));
            return;
        }
        let (sender, cancel) = self.begin_job();
        self.status = "Поиск полезной нагрузки...".to_owned();
        thread::spawn(move || {
            let progress_sender = sender.clone();
            let progress = move |percent: f32, text: &str| {
                let _ = progress_sender.send(WorkerMessage::Progress(
                    (percent / 100.0).clamp(0.0, 1.0),
                    text.to_owned(),
                ));
            };
            let job = Job::new(&progress, &cancel);
            let result = job
                .check()
                .and_then(|()| payload::inspect_file(&carrier, None));
            let _ = sender.send(WorkerMessage::Inspected(result));
        });
    }

    fn start_embed(&mut self) {
        if self.carriers.is_empty() {
            self.result_dialog = Some((false, "Выберите файл-носитель.".to_owned(), None));
            return;
        }
        if self.payloads.is_empty() {
            self.result_dialog = Some((
                false,
                "Выберите файл с полезной нагрузкой.".to_owned(),
                None,
            ));
            return;
        }
        if let Some(bad) = self
            .carriers
            .iter()
            .chain(self.payloads.iter())
            .find(|path| !path.is_file())
        {
            self.result_dialog = Some((false, format!("Файл не найден: {}", bad.display()), None));
            return;
        }

        // Один носитель и одна нагрузка — привычный диалог с именем результата.
        // Пакетный режим пишет рядом с носителями или в выбранную папку.
        let mut planned: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut reservations = Reservations::new();
        if self.carriers.len() == 1 && self.payloads.len() == 1 {
            let carrier = self.carriers[0].clone();
            let suggested = self.embed_suggested_path(&carrier);
            let Some(output) = rfd::FileDialog::new()
                .set_directory(carrier.parent().unwrap_or_else(|| Path::new(".")))
                .set_file_name(
                    suggested
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("carrier_with_payload"),
                )
                .save_file()
            else {
                return;
            };
            planned.push((carrier, output));
        } else {
            if let Some(directory) = self.out_dir.as_ref() {
                if !directory.is_dir() {
                    if let Err(err) = std::fs::create_dir_all(directory) {
                        self.result_dialog =
                            Some((false, format!("Не удалось создать папку: {err}"), None));
                        return;
                    }
                }
            }
            for carrier in &self.carriers {
                let suggested = self.embed_suggested_path(carrier);
                let candidate = match self.out_dir.as_ref() {
                    Some(directory) => directory.join(
                        suggested
                            .file_name()
                            .map(Path::new)
                            .unwrap_or_else(|| Path::new("carrier_with_payload")),
                    ),
                    None => suggested,
                };
                planned.push((carrier.clone(), reservations.reserve(&candidate)));
            }
        }

        let payloads = self.payloads.clone();
        let options = ContainerOptions {
            compress: self.embed_compress,
            password: password_or_none(&self.embed_password),
            bundle: false,
        };
        let copies = self.embed_copies.clamp(1, 3);
        let (sender, cancel) = self.begin_job();
        self.status = "Встраивание полезной нагрузки...".to_owned();
        thread::spawn(move || {
            let total = planned.len().max(1) as f32;
            let mut done = 0_usize;
            let mut failed: Vec<String> = Vec::new();
            let mut last_output: Option<PathBuf> = None;
            let mut last_note = String::new();
            for (index, (carrier, output)) in planned.iter().enumerate() {
                let progress_sender = sender.clone();
                let progress = move |percent: f32, text: &str| {
                    let within = percent.clamp(0.0, 100.0) / 100.0;
                    let overall = ((index as f32 + within) / total).clamp(0.0, 1.0);
                    let _ = progress_sender.send(WorkerMessage::Progress(overall, text.to_owned()));
                };
                let job = Job::new(&progress, &cancel);
                let result = if payloads.len() > 1 {
                    payload::embed_bundle(carrier, &payloads, output, &options, copies, &job)
                } else {
                    payload::embed_file(carrier, &payloads[0], output, &options, copies, &job)
                };
                match result {
                    Ok(outcome) => {
                        done += 1;
                        last_note = format!(
                            "Носитель: {}\nКопий: {}\nОпции: {}\nПроверка: {}\nИтоговый размер: {:.2} МБ",
                            outcome.carrier.label(),
                            outcome.copies,
                            outcome.options,
                            if outcome.verified_with_decoder {
                                "файл заново открывается без ошибок"
                            } else {
                                "декодер не смог проверить файл"
                            },
                            outcome.output_len as f64 / MIB as f64
                        );
                        last_output = Some(outcome.output);
                    }
                    Err(error) => failed.push(format!("{} — {error}", carrier.display())),
                }
            }
            let result = if failed.is_empty() {
                Ok((
                    format!("Нагрузка встроена.\n{last_note}"),
                    last_output.unwrap_or_else(|| PathBuf::from(".")),
                ))
            } else if done == 0 {
                Err(failed.join("\n"))
            } else {
                Ok((
                    format!(
                        "Готово файлов: {done} из {}.\nОшибки:\n{}",
                        planned.len(),
                        failed.join("\n")
                    ),
                    last_output.unwrap_or_else(|| PathBuf::from(".")),
                ))
            };
            let _ = sender.send(WorkerMessage::Finished(result));
        });
    }

    fn start_extract(&mut self, summary: &PayloadSummary, password: String) {
        let Some(carrier) = self.primary_carrier() else {
            return;
        };
        let password = password_or_none(&password);

        // Бандл распаковывается в папку: файлов внутри несколько.
        if summary.info.bundle {
            let Some(directory) = rfd::FileDialog::new()
                .set_directory(carrier.parent().unwrap_or_else(|| Path::new(".")))
                .pick_folder()
            else {
                return;
            };
            let (sender, cancel) = self.begin_job();
            self.status = "Распаковка бандла...".to_owned();
            self.preview = None;
            thread::spawn(move || {
                let progress_sender = sender.clone();
                let progress = move |percent: f32, text: &str| {
                    let _ = progress_sender.send(WorkerMessage::Progress(
                        (percent / 100.0).clamp(0.0, 1.0),
                        text.to_owned(),
                    ));
                };
                let job = Job::new(&progress, &cancel);
                let result =
                    payload::extract_bundle_to(&carrier, &directory, password.as_deref(), &job)
                        .map(|files| {
                            let mut text = format!(
                                "Бандл распакован.\nФайлов: {}\nПапка: {}",
                                files.len(),
                                directory.display()
                            );
                            for (path, size) in files.iter().take(20) {
                                text.push_str(&format!(
                                    "\n  {} ({:.1} КБ)",
                                    path.file_name()
                                        .and_then(|value| value.to_str())
                                        .unwrap_or("файл"),
                                    *size as f64 / 1024.0
                                ));
                            }
                            (text, directory.clone())
                        });
                let _ = sender.send(WorkerMessage::Finished(result));
            });
            return;
        }

        let suggested = payload::default_extract_path(&carrier, &summary.info.name);
        let Some(destination) = rfd::FileDialog::new()
            .set_directory(carrier.parent().unwrap_or_else(|| Path::new(".")))
            .set_file_name(
                suggested
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("payload.bin"),
            )
            .save_file()
        else {
            return;
        };
        let (sender, cancel) = self.begin_job();
        self.status = "Извлечение полезной нагрузки...".to_owned();
        self.preview = None;
        thread::spawn(move || {
            let progress_sender = sender.clone();
            let progress = move |percent: f32, text: &str| {
                let _ = progress_sender.send(WorkerMessage::Progress(
                    (percent / 100.0).clamp(0.0, 1.0),
                    text.to_owned(),
                ));
            };
            let job = Job::new(&progress, &cancel);
            let result = payload::extract_to(&carrier, &destination, password.as_deref(), &job)
                .map(|outcome| {
                    (
                        format!(
                            "Файл извлечён.\nИмя: {}\nРазмер: {:.2} КБ\nЦелых копий: {}",
                            outcome.name,
                            outcome.size as f64 / 1024.0,
                            outcome.copies
                        ),
                        outcome.output,
                    )
                });
            let _ = sender.send(WorkerMessage::Finished(result));
        });
    }

    fn receive_worker_messages(&mut self) {
        let messages: Vec<WorkerMessage> = match self.worker.as_ref() {
            Some(receiver) => receiver.try_iter().collect(),
            None => return,
        };
        for message in messages {
            match message {
                WorkerMessage::Progress(percent, text) => {
                    self.progress = percent;
                    self.status = text;
                }
                WorkerMessage::Finished(Ok((text, path))) => {
                    self.busy = false;
                    self.worker = None;
                    self.progress = 1.0;
                    self.status = text.lines().next().unwrap_or("Готово").to_owned();
                    self.result_dialog = Some((true, text, Some(path)));
                }
                WorkerMessage::Finished(Err(error)) => {
                    self.busy = false;
                    self.worker = None;
                    if error == CANCELLED_MESSAGE {
                        self.progress = 0.0;
                        self.status = CANCELLED_MESSAGE.to_owned();
                    } else {
                        self.status = "Произошла ошибка.".to_owned();
                        self.result_dialog = Some((false, error, None));
                    }
                }
                WorkerMessage::Inspected(Ok(summary)) => {
                    self.busy = false;
                    self.worker = None;
                    self.progress = 0.0;
                    self.status = "Полезная нагрузка найдена.".to_owned();
                    self.preview = Some(Preview {
                        summary,
                        password: String::new(),
                        error: None,
                    });
                }
                WorkerMessage::Inspected(Err(error)) => {
                    self.busy = false;
                    self.worker = None;
                    self.progress = 0.0;
                    self.status = "Полезная нагрузка не найдена.".to_owned();
                    self.result_dialog = Some((false, error, None));
                }
                WorkerMessage::BatchItemStarted(index) => {
                    if let Some(row) = self.rows.get_mut(index) {
                        row.status = "Обработка...".to_owned();
                        row.ok = None;
                    }
                }
                WorkerMessage::BatchItemDone(index, Ok(size)) => {
                    if let Some(row) = self.rows.get_mut(index) {
                        row.status = format!("Готово: {:.2} МБ", size as f64 / MIB as f64);
                        row.ok = Some(true);
                    }
                }
                WorkerMessage::BatchItemDone(index, Err(error)) => {
                    if let Some(row) = self.rows.get_mut(index) {
                        row.status = format!("Ошибка: {error}");
                        row.ok = Some(false);
                    }
                }
                WorkerMessage::BatchFinished {
                    ok,
                    cancelled,
                    text,
                    folder,
                } => {
                    self.busy = false;
                    self.worker = None;
                    self.progress = if ok { 1.0 } else { 0.0 };
                    self.status = if ok {
                        "Пакет обработан.".to_owned()
                    } else if cancelled {
                        CANCELLED_MESSAGE.to_owned()
                    } else {
                        "Часть файлов не удалось обработать.".to_owned()
                    };
                    if cancelled {
                        for row in self.rows.iter_mut() {
                            if row.ok.is_none() {
                                row.status = "Отменено".to_owned();
                                row.ok = Some(false);
                            }
                        }
                    }
                    self.result_dialog = Some((ok, text, folder));
                }
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        // Папки раскрываются рекурсивно; все файлы попадают в пакет, а первые два
        // дополнительно занимают слоты «носитель» и «нагрузка» для встраивания.
        let expanded = scan::expand_dropped(&dropped);
        for path in expanded.iter() {
            if self.carriers.is_empty() {
                self.carriers.push(path.clone());
            } else if self.payloads.is_empty() {
                self.payloads.push(path.clone());
            }
        }
        self.add_files(expanded);
        self.status = "Файлы добавлены перетаскиванием.".to_owned();
    }
}

impl eframe::App for PumperApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_worker_messages();
        self.handle_dropped_files(ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            self.cancel.cancel();
        }

        egui::TopBottomPanel::bottom("file_pumper_status").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(self.status.as_str()).strong());
                if ui
                    .add_enabled(self.busy, egui::Button::new("Отмена"))
                    .clicked()
                {
                    self.cancel.cancel();
                }
            });
            ui.add(
                egui::ProgressBar::new(self.progress.clamp(0.0, 1.0))
                    .show_percentage()
                    .desired_width(ui.available_width()),
            );
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("FilePumper");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak("файлы можно перетаскивать в окно мышью");
                });
            });
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    let selected = self.tab == tab;
                    if ui
                        .selectable_label(selected, egui::RichText::new(tab.title()).size(16.0))
                        .clicked()
                    {
                        self.tab = tab;
                    }
                }
            });
            ui.weak(self.tab.hint());
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.tab {
                    Tab::Pump => self.pump_tab_ui(ui),
                    Tab::Payload => self.payload_tab_ui(ui),
                    Tab::Generate => self.generate_tab_ui(ui),
                });
        });

        self.show_result_dialog(ctx);
        self.show_preview_dialog(ctx);
    }

    /// Сохраняет настройки между запусками (пароль не сохраняется).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings());
    }
}

/// Рамка-группа с заголовком: одинаковое оформление всех блоков окна.
fn group_ui(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.label(egui::RichText::new(title).strong());
        add(ui);
    });
    ui.add_space(6.0);
}

/// Кнопка-действие: крупная, чтобы её было видно сразу.
fn action_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> bool {
    ui.add_enabled(
        enabled,
        egui::Button::new(egui::RichText::new(text).size(17.0)).min_size(egui::vec2(200.0, 30.0)),
    )
    .clicked()
}

impl PumperApp {
    /// Вкладка «Накачать файлы»: список файлов, размер, метод, имена, запуск.
    fn pump_tab_ui(&mut self, ui: &mut egui::Ui) {
        group_ui(ui, "1. Что накачиваем", |ui| self.files_ui(ui));
        group_ui(ui, "2. До какого размера", |ui| {
            self.size_ui(ui)
        });
        group_ui(ui, "3. Как увеличивать", |ui| {
            for method in Method::ALL {
                ui.radio_value(&mut self.method, method, method.title());
            }
            ui.add_enabled_ui(self.method == Method::Animation, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Формат анимации:");
                    egui::ComboBox::from_id_salt("animation_format")
                        .selected_text(match self.animation_format {
                            AnimationFormat::Gif => ".gif",
                            AnimationFormat::Png => ".png (APNG)",
                            AnimationFormat::Webp => ".webp",
                        })
                        .show_ui(ui, |ui| {
                            for (format, label) in [
                                (AnimationFormat::Gif, ".gif"),
                                (AnimationFormat::Png, ".png (APNG)"),
                                (AnimationFormat::Webp, ".webp"),
                            ] {
                                ui.selectable_value(&mut self.animation_format, format, label);
                            }
                        });
                });
            });
        });
        group_ui(
            ui,
            "4. Имена и папка результатов",
            |ui| {
                self.names_ui(ui);
                self.out_dir_ui(ui);
            },
        );
        ui.horizontal(|ui| {
            let ready = !self.busy && !self.rows.is_empty();
            if action_button(ui, ready, "Накачать") {
                self.start_pump();
            }
            ui.weak(match self.rows.len() {
                0 => "сначала добавьте файлы".to_owned(),
                count => format!("файлов к обработке: {count}"),
            });
        });
    }

    /// Список файлов пакета: добавление, удаление и состояние каждого файла.
    fn files_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!self.busy, egui::Button::new("Добавить файлы..."))
                .clicked()
            {
                if let Some(paths) = rfd::FileDialog::new().pick_files() {
                    self.add_files(paths);
                }
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new("Добавить папку..."))
                .on_hover_text("Папка добавляется вместе с подпапками")
                .clicked()
            {
                if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                    match scan::collect_from_dir(&directory, true, &[]) {
                        Ok(found) => self.add_files(found),
                        Err(message) => self.result_dialog = Some((false, message, None)),
                    }
                }
            }
            if ui
                .add_enabled(
                    !self.busy && !self.rows.is_empty(),
                    egui::Button::new("Очистить список"),
                )
                .clicked()
            {
                self.rows.clear();
                self.status = "Список файлов очищен.".to_owned();
            }
            match self.rows.len() {
                0 => ui.weak(
                    "файлы не выбраны — перетащите их в окно или нажмите «Добавить файлы...»",
                ),
                count => ui.weak(format!("файлов: {count}")),
            };
        });

        if self.rows.is_empty() {
            return;
        }
        let mut remove: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_salt("batch_rows")
            .max_height(150.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (index, row) in self.rows.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.busy, egui::Button::new("✕")).clicked() {
                            remove = Some(index);
                        }
                        let name = row
                            .input
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("файл");
                        ui.monospace(name)
                            .on_hover_text(row.input.display().to_string());
                        match row.ok {
                            Some(true) => {
                                ui.colored_label(OK_COLOR, &row.status);
                            }
                            Some(false) => {
                                ui.colored_label(ERROR_COLOR, &row.status);
                            }
                            None => {
                                ui.weak(&row.status);
                            }
                        }
                    });
                }
            });
        if let Some(index) = remove {
            self.rows.remove(index);
        }
    }

    /// Размер и разброс: общий блок для накачки и создания файла.
    fn size_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Размер:");
            ui.add_enabled(
                !self.busy,
                egui::TextEdit::singleline(&mut self.target_mb).desired_width(80.0),
            )
            .on_hover_text("В мегабайтах; дробная часть — через точку или запятую: 1.5 или 1,5");
            ui.label("МБ");
            ui.separator();
            ui.label("считать как:");
            egui::ComboBox::from_id_salt("size_mode")
                .selected_text(self.size_mode.label())
                .show_ui(ui, |ui| {
                    for mode in TargetMode::ALL {
                        ui.selectable_value(&mut self.size_mode, mode, mode.title());
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Разброс:");
            ui.add_enabled(
                !self.busy,
                egui::TextEdit::singleline(&mut self.vary_mb).desired_width(60.0),
            )
            .on_hover_text(
                "Насколько размер может отличаться от указанного: 0 — ровно, 5 — плюс-минус 5 МБ",
            );
            ui.label("МБ");
            ui.separator();
            ui.label("направление:");
            egui::ComboBox::from_id_salt("vary_mode")
                .selected_text(self.vary_mode.label())
                .show_ui(ui, |ui| {
                    for mode in JitterMode::ALL {
                        ui.selectable_value(&mut self.vary_mode, mode, mode.title());
                    }
                });
        });
        match self.jitter() {
            Ok(Some(jitter)) => {
                if let Some(hint) = self.size_range_hint() {
                    ui.weak(hint);
                }
                if jitter.mode != JitterMode::Plus {
                    if let Some(base) = self.target_mb_value().map(|mb| (mb * MIB as f64) as u64) {
                        if jitter.spread >= base {
                            ui.colored_label(
                                WARN_COLOR,
                                format!(
                                    "Разброс {} не меньше самого размера — файл может выйти совсем маленьким.",
                                    jitter.label()
                                ),
                            );
                        }
                    }
                }
            }
            Ok(None) => {
                ui.weak("Разброс 0 — размер всегда ровно указанный.");
            }
            Err(message) => {
                ui.colored_label(ERROR_COLOR, message);
            }
        }
        if let Some(mb) = self.target_mb_value() {
            for warning in scan::limits_warnings((mb * MIB as f64) as u64, self.method) {
                ui.colored_label(WARN_COLOR, warning);
            }
        }
    }

    /// Как называть результаты: размер в имени, без размера или случайно.
    fn names_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Имена:");
            for mode in NameMode::ALL {
                ui.radio_value(&mut self.names, mode, mode.label())
                    .on_hover_text(mode.title());
            }
        });
        ui.weak(format!("Пример: {}", self.example_name()));
    }

    /// Папка результатов: по умолчанию файлы ложатся рядом с исходными.
    fn out_dir_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!self.busy, egui::Button::new("Папка результатов..."))
                .clicked()
            {
                if let Some(directory) = rfd::FileDialog::new().pick_folder() {
                    self.out_dir = Some(directory);
                }
            }
            match self.out_dir.as_ref() {
                Some(directory) => ui.monospace(directory.display().to_string()),
                None => ui.weak("рядом с каждым исходным файлом"),
            };
            if self.out_dir.is_some()
                && ui
                    .add_enabled(!self.busy, egui::Button::new("Сбросить"))
                    .clicked()
            {
                self.out_dir = None;
            }
        });
    }

    /// Вкладка «Спрятать / извлечь»: носители, нагрузка, опции контейнера.
    fn payload_tab_ui(&mut self, ui: &mut egui::Ui) {
        group_ui(
            ui,
            "1. Носители (картинки и другие файлы)",
            |ui| {
                path_list_ui(
                    ui,
                    "Выбрать носители...",
                    "носители не выбраны (PNG, JPEG, GIF, WebP, SVG или любой файл)",
                    &[("Изображения", &["png", "jpg", "jpeg", "gif", "webp", "svg"])],
                    self.busy,
                    &mut self.carriers,
                );
            },
        );
        group_ui(ui, "2. Что спрятать (нагрузка)", |ui| {
            path_list_ui(
                ui,
                "Выбрать нагрузку...",
                "файлы, которые нужно спрятать (несколько — упакуются в бандл)",
                &[],
                self.busy,
                &mut self.payloads,
            );
            ui.add_space(4.0);
            ui.add_enabled(
                !self.busy,
                egui::Checkbox::new(
                    &mut self.embed_compress,
                    "Сжимать нагрузку (deflate) перед встраиванием",
                ),
            );
            ui.horizontal(|ui| {
                ui.label("Копий внутри (устойчивость к обрезке):");
                ui.add_enabled(
                    !self.busy,
                    egui::DragValue::new(&mut self.embed_copies).range(1..=3),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Пароль (необязательно):");
                ui.add_enabled(
                    !self.busy,
                    egui::TextEdit::singleline(&mut self.embed_password)
                        .password(true)
                        .hint_text("Argon2id + ChaCha20-Poly1305")
                        .desired_width(240.0),
                );
            });
        });
        group_ui(ui, "3. Имена файлов результата", |ui| {
            self.names_ui(ui)
        });
        ui.horizontal(|ui| {
            let can_embed = !self.busy && !self.carriers.is_empty() && !self.payloads.is_empty();
            if action_button(ui, can_embed, "Спрятать файл") {
                self.start_embed();
            }
            if action_button(
                ui,
                !self.busy && !self.carriers.is_empty(),
                "Проверить и извлечь",
            ) {
                self.start_inspect();
            }
        });
        ui.add_space(4.0);
        ui.weak(
            "Данные кладутся внутрь структуры файла: чанки PNG, сегменты APP15 JPEG, комментарий GIF, RIFF-чанк WebP, metadata SVG. Картинка остаётся рабочей, поэтому сервер принимает файл целиком.",
        );
    }

    /// Вкладка «Создать файл»: размер, тип, имена и необязательное второе дно.
    fn generate_tab_ui(&mut self, ui: &mut egui::Ui) {
        group_ui(ui, "1. Размер нового файла", |ui| {
            self.size_ui(ui)
        });
        group_ui(ui, "2. Тип файла", |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("generated_kind")
                    .selected_text(self.gen_kind.title())
                    .show_ui(ui, |ui| {
                        for kind in GeneratedKind::ALL {
                            ui.selectable_value(&mut self.gen_kind, kind, kind.title());
                        }
                    });
                ui.weak(if self.gen_kind.is_image() {
                    "останется открывающейся картинкой"
                } else {
                    "случайные байты без формата"
                });
            });
        });
        group_ui(ui, "3. Имя, количество и папка", |ui| {
            ui.horizontal(|ui| {
                ui.label("Основа имени:");
                ui.add_enabled(
                    !self.busy,
                    egui::TextEdit::singleline(&mut self.gen_base)
                        .hint_text("file")
                        .desired_width(140.0),
                );
                ui.label("Файлов на размер:");
                ui.add_enabled(
                    !self.busy,
                    egui::DragValue::new(&mut self.gen_count).range(1..=32),
                );
            });
            self.names_ui(ui);
            self.out_dir_ui(ui);
        });
        group_ui(
            ui,
            "4. Второе дно (необязательно)",
            |ui| {
                ui.add_enabled(
                !self.busy && !self.payloads.is_empty(),
                egui::Checkbox::new(
                    &mut self.gen_attach,
                    "Спрятать внутрь нагрузку со вкладки «Спрятать / извлечь»",
                ),
            )
            .on_hover_text(
                "Контейнер встаёт в заранее посчитанное место, поэтому размер файла не изменится",
            );
                match self.payloads.len() {
                    0 => ui.weak("нагрузка не выбрана — вкладка «Спрятать / извлечь»"),
                    count => ui.weak(format!("нагрузок выбрано: {count}")),
                };
            },
        );
        ui.horizontal(|ui| {
            let ready = !self.busy && self.jitter().is_ok();
            if action_button(ui, ready, "Создать файл") {
                self.start_generate();
            }
            ui.weak(format!("пример имени: {}", self.generated_example_name()));
        });
    }
}

impl PumperApp {
    fn show_result_dialog(&mut self, ctx: &egui::Context) {
        let Some((success, text, path)) = self.result_dialog.clone() else {
            return;
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new(if success {
            "Готово"
        } else {
            "Ошибка"
        })
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(text);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if let Some(path) = path.as_ref() {
                    if ui.button("Показать в проводнике").clicked() {
                        reveal_in_explorer(path);
                    }
                }
                if ui.button("Закрыть").clicked() {
                    close = true;
                }
            });
        });
        if !open || close {
            self.result_dialog = None;
        }
    }

    fn show_preview_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut preview) = self.preview.take() else {
            return;
        };
        let info = preview.summary.info.clone();
        let carrier = preview.summary.carrier;
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Найдена полезная нагрузка")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("Имя файла: {}", info.name));
                ui.label(format!("Размер: {:.2} КБ", info.size as f64 / 1024.0));
                ui.label(format!("Целых копий: {}", info.copies));
                ui.label(format!(
                    "Сжатие: {}",
                    if info.compressed {
                        "да (deflate)"
                    } else {
                        "нет"
                    }
                ));
                ui.label(format!(
                    "Шифрование: {}",
                    if info.encrypted {
                        "пароль"
                    } else {
                        "нет"
                    }
                ));
                if info.legacy {
                    ui.label("Формат: старая версия контейнера (v1)");
                }
                if info.bundle {
                    ui.label("Содержимое: бандл — несколько файлов, будут распакованы в папку");
                }
                ui.label(format!("Носитель: {}", carrier.label()));
                ui.weak(carrier.survives_upload());
                if info.encrypted {
                    ui.horizontal(|ui| {
                        ui.label("Пароль:");
                        ui.add(
                            egui::TextEdit::singleline(&mut preview.password)
                                .password(true)
                                .desired_width(180.0),
                        );
                    });
                }
                if let Some(message) = preview.error.as_ref() {
                    ui.colored_label(egui::Color32::RED, message);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Сохранить как...").clicked() {
                        save = true;
                    }
                    if ui.button("Отмена").clicked() {
                        cancel = true;
                    }
                });
            });
        if save {
            self.start_extract(&preview.summary, preview.password.clone());
        } else if open && !cancel {
            self.preview = Some(preview);
        }
    }
}

fn password_or_none(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Список путей с кнопками «добавить» и «✕»: используется для носителей и
/// нагрузок, чтобы пакетный режим выглядел одинаково.
fn path_list_ui(
    ui: &mut egui::Ui,
    button: &str,
    empty_hint: &str,
    filters: &[(&str, &[&str])],
    busy: bool,
    paths: &mut Vec<PathBuf>,
) {
    ui.horizontal(|ui| {
        if ui.add_enabled(!busy, egui::Button::new(button)).clicked() {
            let mut dialog = rfd::FileDialog::new();
            for &(name, extensions) in filters {
                dialog = dialog.add_filter(name, extensions);
            }
            if let Some(picked) = dialog.pick_files() {
                for path in picked {
                    if !paths
                        .iter()
                        .any(|existing| crate::util::same_file(existing, &path))
                    {
                        paths.push(path);
                    }
                }
            }
        }
        if ui
            .add_enabled(!busy && !paths.is_empty(), egui::Button::new("Очистить"))
            .clicked()
        {
            paths.clear();
        }
        match paths.len() {
            0 => ui.weak(empty_hint),
            count => ui.weak(format!("выбрано: {count}")),
        };
    });
    let mut remove: Option<usize> = None;
    for (index, path) in paths.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("✕")).clicked() {
                remove = Some(index);
            }
            ui.monospace(
                path.file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("файл"),
            )
            .on_hover_text(path.display().to_string());
        });
    }
    if let Some(index) = remove {
        paths.remove(index);
    }
}

/// Запускает графический интерфейс.
pub fn run_gui() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        hardware_acceleration: eframe::HardwareAcceleration::Preferred,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([880.0, 780.0])
            .with_min_inner_size([680.0, 520.0])
            .with_resizable(true),
        ..Default::default()
    };
    eframe::run_native(
        "FilePumper",
        options,
        Box::new(|cc| Ok(Box::new(PumperApp::new(cc)))),
    )
}
