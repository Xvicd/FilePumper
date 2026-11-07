//! Консольная версия для терминала: команды `pump`, `embed`, `extract`, `inspect`.
//! Собрана как обычное консольное приложение, поэтому вывод попадает прямо в
//! окно терминала (в отличие от графической сборки).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(file_pumper::cli::run(&args) as u8)
}
