//! Нагрузка внутри PDF: неиспользуемый поток-объект перед `%%EOF`.
//!
//! Объект не указан в xref, поэтому программы его не читают и документ
//! открывается как обычно. При извлечении контейнер ищется по маркеру.

use std::fs;
use std::path::Path;

use crate::util::Job;

/// Номер неиспользуемого объекта: он не должен пересекаться с обычными.
const OBJECT_NUMBER: u32 = 999_999;

/// Читает весь файл: контейнер ищется по маркеру в потоке.
pub fn extract(carrier: &Path) -> Result<Vec<u8>, String> {
    fs::read(carrier).map_err(|err| err.to_string())
}

/// Вставляет поток-объект с нагрузкой перед последним `%%EOF`.
pub fn embed(carrier: &Path, data: &[u8], output: &Path, job: &Job<'_>) -> Result<(), String> {
    job.check()?;
    let bytes = fs::read(carrier).map_err(|err| err.to_string())?;
    if !bytes.starts_with(b"%PDF") {
        return Err("Файл не является PDF.".to_owned());
    }
    let eof = rfind(&bytes, b"%%EOF").unwrap_or(bytes.len());
    let mut result = Vec::with_capacity(bytes.len() + data.len() + 96);
    result.extend_from_slice(&bytes[..eof]);
    result.extend_from_slice(
        format!(
            "%FilePumper payload\n{OBJECT_NUMBER} 0 obj\n<< /Length {} >>\nstream\n",
            data.len()
        )
        .as_bytes(),
    );
    result.extend_from_slice(data);
    result.extend_from_slice(b"\nendstream\nendobj\n");
    result.extend_from_slice(&bytes[eof..]);
    fs::write(output, &result).map_err(|err| err.to_string())?;
    Ok(())
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{detect_carrier, extract_payload, verify_file, Carrier};

    fn sample_pdf() -> Vec<u8> {
        let mut body = b"%PDF-1.4\n".to_vec();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        let xref = body.len();
        body.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        body.extend_from_slice(format!("{xref:010} 00000 n \n").as_bytes());
        body.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
        );
        body
    }

    #[test]
    fn payload_is_written_as_stream_and_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = dir.path().join("doc.pdf");
        fs::write(&carrier, sample_pdf()).unwrap();
        assert_eq!(detect_carrier(&carrier).unwrap(), Carrier::Pdf);

        let output = dir.path().join("with_payload.pdf");
        let outcome = crate::payload::embed_bytes(
            &carrier,
            b"secret inside pdf",
            "secret.txt",
            &output,
            &crate::payload::ContainerOptions::default(),
            1,
            &Job::quiet(),
        )
        .unwrap();
        assert_eq!(outcome.carrier, Carrier::Pdf);

        let bytes = fs::read(&output).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes
            .windows(b"%FilePumper".len())
            .any(|window| window == b"%FilePumper"));
        // %%EOF по-прежнему в конце файла.
        assert!(bytes.ends_with(b"%%EOF\n") || bytes.ends_with(b"%%EOF"));

        let (parsed, copies, kind) = extract_payload(&output, None).unwrap();
        assert_eq!(kind, Carrier::Pdf);
        assert_eq!(copies, 1);
        assert_eq!(parsed.payload, b"secret inside pdf");
        assert_eq!(verify_file(&output, None).unwrap().name, "secret.txt");
    }

    #[test]
    fn extraction_works_when_container_has_copies() {
        let dir = tempfile::tempdir().unwrap();
        let carrier = dir.path().join("doc.pdf");
        fs::write(&carrier, sample_pdf()).unwrap();
        let output = dir.path().join("multi.pdf");
        crate::payload::embed_bytes(
            &carrier,
            b"three copies",
            "secret.txt",
            &output,
            &crate::payload::ContainerOptions::default(),
            3,
            &Job::quiet(),
        )
        .unwrap();
        let (parsed, copies, _) = extract_payload(&output, None).unwrap();
        assert_eq!(copies, 3);
        assert_eq!(parsed.payload, b"three copies");
    }
}
