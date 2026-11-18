//! Text extraction for the three supported file types. Input is untrusted: PDF parsing runs on a
//! dedicated thread so a panic in the parser (or its dependencies) fails one file instead of the
//! app.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::normalize::normalize;

/// Largest file the core reads.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Parsing recurses through the page tree and nested objects; the default 2 MiB thread stack is
/// too small for deep but legitimate documents.
const PDF_STACK_BYTES: usize = 32 * 1024 * 1024;
const PDF_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Text,
    Markdown,
    Pdf,
}

impl FileKind {
    /// Decided by the extension only, case-insensitively.
    #[must_use]
    pub fn from_file_name(name: &str) -> Option<Self> {
        let (_, ext) = name.rsplit_once('.')?;
        match ext.to_ascii_lowercase().as_str() {
            "txt" => Some(Self::Text),
            "md" | "markdown" => Some(Self::Markdown),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractError {
    #[error("only .txt, .md and .pdf files can be used")]
    Unsupported,
    #[error("the file is larger than 10 MB")]
    TooLarge,
    #[error("the PDF could not be read")]
    BadPdf,
    #[error("the file has no text")]
    NoText,
}

/// Returns normalized text. Text and Markdown files are decoded as UTF-8, UTF-16 (with a byte
/// order mark) or, failing both, Windows-1252, which is what older Portuguese text files use.
pub fn extract(kind: FileKind, bytes: &[u8]) -> Result<String, ExtractError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(ExtractError::TooLarge);
    }
    let raw = match kind {
        FileKind::Text | FileKind::Markdown => decode_text(bytes),
        FileKind::Pdf => pdf_text(bytes)?,
    };
    let text = normalize(&raw);
    if text.is_empty() {
        return Err(ExtractError::NoText);
    }
    Ok(text)
}

fn decode_text(bytes: &[u8]) -> String {
    if let Some((encoding, _)) = encoding_rs::Encoding::for_bom(bytes) {
        return encoding.decode(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn pdf_text(bytes: &[u8]) -> Result<String, ExtractError> {
    let owned = bytes.to_vec();
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("pdf-extract".into())
        .stack_size(PDF_STACK_BYTES)
        .spawn(move || {
            let _ = tx.send(pdf_extract::extract_text_from_mem(&owned));
        })
        .map_err(|_| ExtractError::BadPdf)?;
    // A panic drops the sender and ends the wait at once; a parser stuck in a loop is abandoned
    // after the timeout (its thread is leaked, which beats a frozen import).
    match rx.recv_timeout(PDF_TIMEOUT) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(_)) | Err(_) => Err(ExtractError::BadPdf),
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod fixtures {
    use std::fmt::Write as _;

    /// A one-page-per-string PDF using the standard Helvetica font and WinAnsi encoding, so
    /// accented Portuguese letters are written as octal escapes.
    pub fn pdf(pages: &[&str]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let page_count = pages.len();
        let first_page_obj = 4;
        let kids: String = (0..page_count)
            .map(|i| format!("{} 0 R", first_page_obj + 2 * i))
            .collect::<Vec<_>>()
            .join(" ");
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        objects.push(format!(
            "<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"
        ));
        objects.push(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .into(),
        );
        for (i, text) in pages.iter().enumerate() {
            let content_obj = first_page_obj + 2 * i + 1;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R >> >> /Contents {content_obj} 0 R >>"
            ));
            let mut stream = String::from("BT /F1 12 Tf 72 720 Td 14 TL\n");
            for line in text.lines() {
                stream.push('(');
                for c in line.chars() {
                    match c {
                        '(' | ')' | '\\' => {
                            stream.push('\\');
                            stream.push(c);
                        }
                        c if c.is_ascii() => stream.push(c),
                        c => {
                            let _ = write!(stream, "\\{:03o}", u32::from(c));
                        }
                    }
                }
                stream.push_str(") Tj T*\n");
            }
            stream.push_str("ET");
            objects.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            let _ = write!(out, "{} 0 obj\n{body}\nendobj\n", i + 1);
        }
        let xref = out.len();
        let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for o in offsets {
            let _ = writeln!(out, "{o:010} 00000 n ");
        }
        let _ = write!(
            out,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        );
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(FileKind::from_file_name("a.TXT"), Some(FileKind::Text));
        assert_eq!(
            FileKind::from_file_name("notes.md"),
            Some(FileKind::Markdown)
        );
        assert_eq!(FileKind::from_file_name("x.y.pdf"), Some(FileKind::Pdf));
        assert_eq!(FileKind::from_file_name("a.docx"), None);
        assert_eq!(FileKind::from_file_name("noextension"), None);
        assert_eq!(FileKind::from_file_name("pdf"), None);
        assert_eq!(FileKind::from_file_name("a.pdf.exe"), None);
    }

    #[test]
    fn plain_text_is_normalized() {
        let text = extract(FileKind::Text, b"Line one\r\n\r\n\r\n\r\nLine   two\n").unwrap();
        assert_eq!(text, "Line one\n\nLine two");
    }

    #[test]
    fn markdown_is_kept_as_written() {
        let text = extract(FileKind::Markdown, b"# Title\n\n- item *one*\n").unwrap();
        assert_eq!(text, "# Title\n\n- item *one*");
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("Olá".as_bytes());
        assert_eq!(extract(FileKind::Text, &bytes).unwrap(), "Olá");
    }

    #[test]
    fn utf16_with_bom_is_decoded() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "Olá mundo".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(extract(FileKind::Text, &bytes).unwrap(), "Olá mundo");
    }

    #[test]
    fn windows_1252_text_is_decoded() {
        let bytes = b"A\xe7\xe3o e cora\xe7\xe3o";
        assert_eq!(extract(FileKind::Text, bytes).unwrap(), "Ação e coração");
    }

    #[test]
    fn empty_and_blank_files_have_no_text() {
        assert_eq!(extract(FileKind::Text, b""), Err(ExtractError::NoText));
        assert_eq!(
            extract(FileKind::Markdown, b" \n\t\n"),
            Err(ExtractError::NoText)
        );
    }

    #[test]
    fn oversized_input_is_refused_before_parsing() {
        let big = vec![b'a'; usize::try_from(MAX_FILE_BYTES).unwrap_or(0) + 1];
        assert_eq!(extract(FileKind::Text, &big), Err(ExtractError::TooLarge));
        assert_eq!(extract(FileKind::Pdf, &big), Err(ExtractError::TooLarge));
    }

    #[test]
    fn pdf_text_is_extracted_with_accents() {
        let bytes =
            fixtures::pdf(&["Contrato de prestação de serviços\nValor mensal: 1.500 reais"]);
        let text = extract(FileKind::Pdf, &bytes).unwrap();
        assert!(text.contains("prestação de serviços"), "{text:?}");
        assert!(text.contains("Valor mensal"), "{text:?}");
    }

    #[test]
    fn pdf_pages_are_all_read() {
        let bytes = fixtures::pdf(&["First page about pricing", "Second page about refunds"]);
        let text = extract(FileKind::Pdf, &bytes).unwrap();
        assert!(
            text.contains("pricing") && text.contains("refunds"),
            "{text:?}"
        );
    }

    #[test]
    fn garbage_is_not_a_pdf() {
        assert_eq!(
            extract(FileKind::Pdf, b"definitely not a pdf"),
            Err(ExtractError::BadPdf)
        );
        assert_eq!(extract(FileKind::Pdf, b""), Err(ExtractError::BadPdf));
    }

    #[test]
    fn a_truncated_pdf_fails_cleanly() {
        let bytes = fixtures::pdf(&["Some text"]);
        let cut = &bytes[..bytes.len() / 2];
        assert!(extract(FileKind::Pdf, cut).is_err());
    }
}
