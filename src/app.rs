//! Графический интерфейс: накачка файлов, встраивание и извлечение нагрузки.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui;

use crate::batch::{self, BatchItem};
use crate::payload::{self, ContainerOptions, PayloadSummary};
use crate::pumper::{AnimationFormat, Method};
use crate::util::{reveal_in_explorer, CancelToken, Job, CANCELLED_MESSAGE, MIB};

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

pub struct PumperApp {
    rows: Vec<BatchRow>,
    out_dir: Option<PathBuf>,
    method: Method,
    animation_format: AnimationFormat,
    target_mb: String,
    status: String,
    progress: f32,
    worker: Option<Receiver<WorkerMessage>>,
    cancel: CancelToken,
    busy: bool,
    result_dialog: Option<(bool, String, Option<PathBuf>)>,
    carrier_path: Option<PathBuf>,
    payload_path: Option<PathBuf>,
    embed_compress: bool,
    embed_password: String,
    embed_copies: usize,
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
            status: "Добавьте файлы и режим, затем нажмите «Запустить».".to_owned(),
            progress: 0.0,
            worker: None,
            cancel: CancelToken::new(),
            busy: false,
            result_dialog: None,
            carrier_path: None,
            payload_path: None,
            embed_compress: false,
            embed_password: String::new(),
            embed_copies: 1,
            preview: None,
        }
    }
}

fn channel() -> (mpsc::Sender<WorkerMessage>, Receiver<WorkerMessage>) {
    mpsc::channel()
}

impl PumperApp {
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
        let bytes_f64 = mb * MIB as f64;
        if bytes_f64 > u64::MAX as f64 {
            self.result_dialog = Some((false, "Указанный размер слишком велик.".to_owned(), None));
            return;
        }
        let target_bytes = bytes_f64 as u64;
        let method = self.method;
        let format = self.animation_format;
        let out_dir = self.out_dir.clone();
        if let Some(directory) = out_dir.as_ref() {
            if !directory.is_dir() {
                if let Err(err) = std::fs::create_dir_all(directory) {
                    self.result_dialog =
                        Some((false, format!("Не удалось создать папку: {err}"), None));
                    return;
                }
            }
        }

        let mut work: Vec<(usize, BatchItem)> = Vec::new();
        for (index, row) in self.rows.iter_mut().enumerate() {
            row.ok = None;
            match BatchItem::new(&row.input, target_bytes, method, format, out_dir.as_deref()) {
                Ok(item) => {
                    row.status = "Ожидает".to_owned();
                    work.push((index, item));
                }
                Err(message) => {
                    row.status = format!("Пропуск: {message}");
                    row.ok = Some(false);
                }
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
                match batch::pump_one(&item, method, format, target_bytes, &item_job) {
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

    fn start_inspect(&mut self) {
        let Some(carrier) = self.carrier_path.clone() else {
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
        let Some(carrier) = self.carrier_path.clone() else {
            self.result_dialog = Some((false, "Выберите файл-носитель.".to_owned(), None));
            return;
        };
        let Some(payload) = self.payload_path.clone() else {
            self.result_dialog = Some((
                false,
                "Выберите файл с полезной нагрузкой.".to_owned(),
                None,
            ));
            return;
        };
        if !carrier.is_file() || !payload.is_file() {
            self.result_dialog = Some((false, "Проверьте выбранные файлы.".to_owned(), None));
            return;
        }
        let suggested = payload::default_output_path(&carrier);
        let Some(output) = rfd::FileDialog::new()
            .set_directory(
                carrier
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )
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
        let options = ContainerOptions {
            compress: self.embed_compress,
            password: password_or_none(&self.embed_password),
        };
        let copies = self.embed_copies.clamp(1, 3);
        let (sender, cancel) = self.begin_job();
        self.status = "Встраивание полезной нагрузки...".to_owned();
        thread::spawn(move || {
            let progress_sender = sender.clone();
            let progress = move |percent: f32, text: &str| {
                let _ = progress_sender.send(WorkerMessage::Progress(
                    (percent / 100.0).clamp(0.0, 1.0),
                    text.to_owned(),
                ));
            };
            let job = Job::new(&progress, &cancel);
            let result = payload::embed_file(&carrier, &payload, &output, &options, copies, &job).map(
                |outcome| {
                    let check = if outcome.verified_with_decoder {
                        "файл заново открывается без ошибок"
                    } else {
                        "декодер не смог проверить файл"
                    };
                    (
                        format!(
                            "Нагрузка встроена.\nНоситель: {}\nКопий: {}\nОпции: {}\nПроверка: {}\nИтоговый размер: {:.2} МБ",
                            outcome.carrier.label(),
                            outcome.copies,
                            outcome.options,
                            check,
                            outcome.output_len as f64 / MIB as f64
                        ),
                        outcome.output,
                    )
                },
            );
            let _ = sender.send(WorkerMessage::Finished(result));
        });
    }

    fn start_extract(&mut self, summary: &PayloadSummary, password: String) {
        let Some(carrier) = self.carrier_path.clone() else {
            return;
        };
        let suggested = payload::default_extract_path(&carrier, &summary.info.name);
        let Some(destination) = rfd::FileDialog::new()
            .set_directory(
                carrier
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new(".")),
            )
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
        let password = password_or_none(&password);
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
        // Все перетащенные файлы попадают в пакет, а первые два дополнительно
        // занимают слоты «носитель» и «нагрузка» для встраивания.
        for path in dropped.iter() {
            if self.carrier_path.is_none() {
                self.carrier_path = Some(path.clone());
            } else if self.payload_path.is_none() {
                self.payload_path = Some(path.clone());
            } else {
                self.carrier_path = Some(path.clone());
            }
        }
        self.add_files(dropped);
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

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.heading("FilePumper");
                    ui.label("Добавляет файлу объём и умеет прятать в него другой файл.");
                    ui.weak("Файлы можно перетаскивать в окно мышью.");
                    ui.separator();

                    ui.label("1. Исходные файлы (можно сразу несколько):");
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Выбрать файлы..."))
                            .clicked()
                        {
                            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                                self.add_files(paths);
                            }
                        }
                        if ui
                            .add_enabled(!self.busy && !self.rows.is_empty(), egui::Button::new("Очистить"))
                            .clicked()
                        {
                            self.rows.clear();
                            self.status = "Список файлов очищен.".to_owned();
                        }
                        match self.rows.len() {
                            0 => ui.weak("файлы не выбраны"),
                            count => ui.weak(format!("файлов: {count}")),
                        };
                    });

                    if !self.rows.is_empty() {
                        let mut remove: Option<usize> = None;
                        egui::ScrollArea::vertical()
                            .id_salt("batch_rows")
                            .max_height(160.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for (index, row) in self.rows.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_enabled(!self.busy, egui::Button::new("✕"))
                                            .clicked()
                                        {
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
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(0x3c, 0xb3, 0x71),
                                                    &row.status,
                                                );
                                            }
                                            Some(false) => {
                                                ui.colored_label(
                                                    egui::Color32::from_rgb(0xd9, 0x53, 0x4f),
                                                    &row.status,
                                                );
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

                    ui.add_space(6.0);
                    ui.label("2. Метод увеличения:");
                    for method in Method::ALL {
                        ui.radio_value(&mut self.method, method, method.title());
                    }

                    ui.horizontal(|ui| {
                        ui.label("Формат режима 1:");
                        egui::ComboBox::from_id_salt("animation_format")
                            .selected_text(match self.animation_format {
                                AnimationFormat::Gif => ".gif",
                                AnimationFormat::Png => ".png (APNG)",
                                AnimationFormat::Webp => ".webp",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.animation_format,
                                    AnimationFormat::Gif,
                                    ".gif",
                                );
                                ui.selectable_value(
                                    &mut self.animation_format,
                                    AnimationFormat::Png,
                                    ".png (APNG)",
                                );
                                ui.selectable_value(
                                    &mut self.animation_format,
                                    AnimationFormat::Webp,
                                    ".webp",
                                );
                            });
                    });

                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.label("3. Желаемый размер (МБ):");
                        ui.add(egui::TextEdit::singleline(&mut self.target_mb).desired_width(100.0));
                    });

                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
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

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.busy && !self.rows.is_empty(),
                                egui::Button::new("Запустить"),
                            )
                            .clicked()
                        {
                            self.start_pump();
                        }
                        if ui.add_enabled(self.busy, egui::Button::new("Отмена")).clicked() {
                            self.cancel.cancel();
                        }
                    });

                    ui.add_space(6.0);
                    ui.label(self.status.clone());
                    ui.add(
                        egui::ProgressBar::new(self.progress.clamp(0.0, 1.0))
                            .show_percentage(),
                    );

                    ui.add_space(10.0);
                    ui.separator();
                    ui.heading("4. Полезная нагрузка внутри файла");
                    ui.label("Спрячьте любой файл внутрь картинки, а потом извлеките обратно.");

                    ui.horizontal(|ui| {
                        if ui.button("Файл-носитель...").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("Изображения", &["png", "jpg", "jpeg", "gif", "webp", "svg"])
                                .pick_file()
                            {
                                self.carrier_path = Some(path);
                            }
                        }
                        match self.carrier_path.as_ref() {
                            Some(path) => ui.monospace(path.display().to_string()),
                            None => ui.weak("носитель не выбран (PNG, JPEG, GIF, WebP, SVG или любой файл)"),
                        };
                    });

                    ui.horizontal(|ui| {
                        if ui.button("Файл нагрузки...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                self.payload_path = Some(path);
                            }
                        }
                        match self.payload_path.as_ref() {
                            Some(path) => ui.monospace(path.display().to_string()),
                            None => ui.weak("файл, который нужно спрятать"),
                        };
                    });

                    ui.add_space(4.0);
                    ui.checkbox(
                        &mut self.embed_compress,
                        "Сжимать нагрузку (deflate) перед встраиванием",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Копий нагрузки (устойчивость к обрезке сервером):");
                        ui.add(egui::DragValue::new(&mut self.embed_copies).range(1..=3));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Пароль (необязательно):");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.embed_password)
                                .password(true)
                                .hint_text("шифрование Argon2id + ChaCha20-Poly1305")
                                .desired_width(260.0),
                        );
                    });

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let can_embed = !self.busy
                            && self.carrier_path.is_some()
                            && self.payload_path.is_some();
                        if ui
                            .add_enabled(can_embed, egui::Button::new("Вшить файл..."))
                            .clicked()
                        {
                            self.start_embed();
                        }
                        if ui
                            .add_enabled(
                                !self.busy && self.carrier_path.is_some(),
                                egui::Button::new("Проверить и извлечь..."),
                            )
                            .clicked()
                        {
                            self.start_inspect();
                        }
                    });
                    ui.weak(
                        "Данные кладутся внутрь структуры файла: чанки PNG, сегменты APP15 JPEG, комментарий GIF, RIFF-чанк WebP, metadata SVG. Картинка остаётся рабочей, поэтому сервер принимает файл целиком.",
                    );
                });
        });

        self.show_result_dialog(ctx);
        self.show_preview_dialog(ctx);
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
        Box::new(|_creation_context| Ok(Box::<PumperApp>::default())),
    )
}
