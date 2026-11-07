//! Отчёт о пакетном запуске: CSV или JSON, формат выбирается по расширению.

use std::fs;
use std::path::Path;

use crate::batch::{BatchReport, ItemResult};

/// Одна строка отчёта: что обрабатывали, что получилось и почему не вышло.
struct Row<'a> {
    input: &'a Path,
    output: Option<&'a Path>,
    status: &'static str,
    bytes: Option<u64>,
    message: Option<&'a str>,
}

fn rows(report: &BatchReport) -> Vec<Row<'_>> {
    report
        .items
        .iter()
        .map(|(input, result)| match result {
            ItemResult::Created { output, size } => Row {
                input: input.as_path(),
                output: Some(output.as_path()),
                status: "created",
                bytes: Some(*size),
                message: None,
            },
            ItemResult::Failed { message } => Row {
                input: input.as_path(),
                output: None,
                status: "failed",
                bytes: None,
                message: Some(message.as_str()),
            },
            ItemResult::Skipped { reason } => Row {
                input: input.as_path(),
                output: None,
                status: "skipped",
                bytes: None,
                message: Some(reason.as_str()),
            },
        })
        .collect()
}

fn escape_csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// CSV с заголовком: `input,output,status,bytes,message`.
pub fn csv(report: &BatchReport) -> String {
    let mut text = String::from("input,output,status,bytes,message\n");
    for row in rows(report) {
        let number = row.bytes.map(|bytes| bytes.to_string()).unwrap_or_default();
        text.push_str(&format!(
            "{},{},{},{},{}\n",
            escape_csv(&row.input.display().to_string()),
            escape_csv(
                &row.output
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            ),
            row.status,
            number,
            escape_csv(row.message.unwrap_or_default())
        ));
    }
    text
}

fn escape_json(value: &str) -> String {
    let mut text = String::with_capacity(value.len() + 2);
    for symbol in value.chars() {
        match symbol {
            '"' => text.push_str("\\\""),
            '\\' => text.push_str("\\\\"),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            other if other.is_control() => text.push_str(&format!("\\u{:04x}", other as u32)),
            other => text.push(other),
        }
    }
    text
}

fn json_string(value: &str) -> String {
    format!("\"{}\"", escape_json(value))
}

/// JSON: сводка и подробности по каждому файлу.
pub fn json(report: &BatchReport) -> String {
    let mut text = format!(
        "{{\n  \"cancelled\": {},\n  \"created\": {},\n  \"failed\": {},\n  \"skipped\": {},\n  \"items\": [",
        report.cancelled,
        report.created(),
        report.failed(),
        report.skipped()
    );
    for (index, row) in rows(report).iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        let output = row
            .output
            .map(|path| json_string(&path.display().to_string()))
            .unwrap_or_else(|| "null".to_owned());
        let bytes = row
            .bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "null".to_owned());
        let message = row
            .message
            .map(json_string)
            .unwrap_or_else(|| "null".to_owned());
        text.push_str(&format!(
            "\n    {{\"input\": {}, \"output\": {}, \"status\": {}, \"bytes\": {}, \"message\": {}}}",
            json_string(&row.input.display().to_string()),
            output,
            json_string(row.status),
            bytes,
            message
        ));
    }
    text.push_str("\n  ]\n}\n");
    text
}

/// Записывает отчёт: `.json` — JSON, любое другое расширение — CSV.
pub fn write_report(path: &Path, report: &BatchReport) -> Result<(), String> {
    let text = match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => json(report),
        _ => csv(report),
    };
    if let Some(directory) = path.parent() {
        if !directory.as_os_str().is_empty() && !directory.is_dir() {
            fs::create_dir_all(directory).map_err(|err| {
                format!("Не удалось создать папку {}: {err}", directory.display())
            })?;
        }
    }
    fs::write(path, text)
        .map_err(|err| format!("Не удалось записать отчёт {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample() -> BatchReport {
        let mut report = BatchReport::default();
        report.items.push((
            PathBuf::from("C:/in/one.txt"),
            ItemResult::Created {
                output: PathBuf::from("C:/out/one_1MB.txt"),
                size: 1_048_576,
            },
        ));
        report.items.push((
            PathBuf::from("C:/in/two.txt"),
            ItemResult::Failed {
                message: "файл занят".to_owned(),
            },
        ));
        report.items.push((
            PathBuf::from("C:/in/three.txt"),
            ItemResult::Skipped {
                reason: "неподдерживаемый формат".to_owned(),
            },
        ));
        report
    }

    #[test]
    fn csv_has_header_and_quotes_paths() {
        let text = csv(&sample());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "input,output,status,bytes,message");
        assert_eq!(lines.len(), 4);
        assert!(lines[1].contains("\"C:/out/one_1MB.txt\""));
        assert!(lines[1].contains("1048576"));
        assert!(lines[2].contains("failed"));
    }

    #[test]
    fn json_is_valid_and_escapes_values() {
        let text = json(&sample());
        assert!(text.starts_with('{'));
        assert!(text.trim_end().ends_with('}'));
        assert!(text.contains("\"created\": 1"));
        assert!(text.contains("\"failed\": 1"));
        assert!(text.contains("\"skipped\": 1"));
        assert!(text.contains("\"bytes\": 1048576"));
        assert_eq!(escape_json("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }

    #[test]
    fn report_format_follows_extension() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("report.csv");
        write_report(&csv_path, &sample()).unwrap();
        assert!(fs::read_to_string(&csv_path).unwrap().contains("status"));
        let json_path = dir.path().join("nested/report.json");
        write_report(&json_path, &sample()).unwrap();
        assert!(fs::read_to_string(&json_path)
            .unwrap()
            .contains("\"items\""));
    }
}
