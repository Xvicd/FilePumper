//! Графическая версия: без аргументов открывается окно, с аргументами команда
//! выполняется как в CLI. На Windows сборка помечена как оконная
//! (`windows_subsystem = "windows"`), поэтому консоль при двойном клике не
//! появляется; для работы в терминале есть отдельный `file-pumper-cli`.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

use file_pumper::{app, cli};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return match app::run_gui() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&format!(
                "Не удалось запустить графический интерфейс: {error}"
            )),
        };
    }
    match cli::execute(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}

/// У оконной сборки нет консоли, поэтому ошибку показываем диалогом.
#[cfg(windows)]
fn fail(message: &str) -> ExitCode {
    eprintln!("Ошибка: {message}");
    rfd::MessageDialog::new()
        .set_title("FilePumper")
        .set_level(rfd::MessageLevel::Error)
        .set_description(message)
        .show();
    ExitCode::FAILURE
}

#[cfg(not(windows))]
fn fail(message: &str) -> ExitCode {
    eprintln!("Ошибка: {message}");
    ExitCode::FAILURE
}
