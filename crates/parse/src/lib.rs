use anyhow::{Context, Result, anyhow};
use quick_xml::Reader;
use quick_xml::events::Event;
use regex::Regex;
use shortlist_core::{Candidate, now_ms};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const SKILLS: &[&str] = &[
    "Rust",
    "Go",
    "Python",
    "JavaScript",
    "TypeScript",
    "React",
    "Next.js",
    "Node.js",
    "Java",
    "Kotlin",
    "Swift",
    "C++",
    "C#",
    ".NET",
    "Ruby",
    "PHP",
    "Scala",
    "Elixir",
    "AWS",
    "Azure",
    "GCP",
    "Docker",
    "Kubernetes",
    "Terraform",
    "Ansible",
    "Linux",
    "PostgreSQL",
    "MySQL",
    "SQLite",
    "MongoDB",
    "Redis",
    "Elasticsearch",
    "Snowflake",
    "Kafka",
    "RabbitMQ",
    "gRPC",
    "GraphQL",
    "REST",
    "Microservices",
    "Distributed Systems",
    "Machine Learning",
    "Deep Learning",
    "NLP",
    "LLM",
    "PyTorch",
    "TensorFlow",
    "MLOps",
    "Data Analysis",
    "Data Engineering",
    "SQL",
    "Spark",
    "Airflow",
    "dbt",
    "Tableau",
    "Power BI",
    "Figma",
    "UI/UX",
    "Design Systems",
    "User Research",
    "Prototyping",
    "Product Management",
    "Project Management",
    "Agile",
    "Scrum",
    "DevOps",
    "CI/CD",
    "Observability",
    "Security",
    "Cryptography",
    "Threat Modeling",
    "Salesforce",
    "SAP",
    "Excel",
    "Finance",
    "Accounting",
    "Recruiting",
    "HR",
    "Customer Success",
    "Leadership",
    "Hiring",
    "Roadmapping",
    "Analytics",
    "Accessibility",
    "CSS",
    "HTML",
];

pub fn extract_text(path: &Path) -> Result<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "pdf" => extract_pdf(path),
        "docx" => extract_docx(path),
        "txt" | "md" | "markdown" | "rtf" => {
            std::fs::read_to_string(path).context("read text file")
        }
        "doc" => Err(anyhow!(
            "Legacy .doc is not supported. Save it as .docx and import again."
        )),
        _ => std::fs::read_to_string(path).context("read file"),
    }
}

/// Parse PDFs in a child process so a malformed file can never abort the app.
fn extract_pdf(path: &Path) -> Result<String> {
    if let Some(helper) = pdf_helper() {
        let output = Command::new(&helper).arg(path).output();
        if let Ok(output) = output {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout).to_string();
                if text.trim().is_empty() {
                    return Err(anyhow!(
                        "No text found in this PDF. It may be scanned; OCR is not enabled yet."
                    ));
                }
                return Ok(text);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr
                .lines()
                .last()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .unwrap_or("the parser rejected the file");
            return Err(anyhow!("Could not parse this PDF: {reason}"));
        }
    }

    // Fallback for tests and unusual installs. A panic here is contained by
    // the caller's catch_unwind, but the child process path is preferred.
    let owned = path.to_path_buf();
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pdf_extract::extract_text(&owned)
    })) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(err)) => Err(anyhow!(err)),
        Err(_) => Err(anyhow!("The PDF parser crashed on this file.")),
    }
}

fn pdf_helper() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SHORTLIST_PDF_HELPER") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) {
        "shortlist-pdf.exe"
    } else {
        "shortlist-pdf"
    };
    let mut candidates = vec![dir.join(name)];
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join(name));
    }
    candidates.into_iter().find(|path| path.exists())
}

fn extract_docx(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut doc = zip
        .by_name("word/document.xml")
        .context("docx has no word/document.xml")?;
    let mut xml = String::new();
    doc.read_to_string(&mut xml)?;

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"w:t" => in_text = true,
            Ok(Event::End(e)) if e.name().as_ref() == b"w:t" => in_text = false,
            Ok(Event::End(e)) if e.name().as_ref() == b"w:p" => out.push('\n'),
            Ok(Event::Text(t)) if in_text => {
                out.push_str(&t.unescape().unwrap_or_default());
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow!("docx xml: {e}")),
            _ => {}
        }
    }
    Ok(out)
}

pub fn clean_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_space = false;
    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            out.push('\n');
            last_space = false;
            continue;
        }
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out.trim().to_string()
}

const SECTION_HEADERS: &[&str] = &[
    "technical skills",
    "work experience",
    "professional experience",
    "employment history",
    "work history",
    "experience",
    "education",
    "summary",
    "professional summary",
    "profile",
    "projects",
    "certifications",
    "certificates",
    "awards",
    "languages",
    "contact",
    "contact information",
    "objective",
    "skills",
    "achievements",
    "publications",
    "interests",
    "references",
    "personal details",
    "declaration",
    "about me",
    "volunteer experience",
];

fn is_section_header(line: &str) -> bool {
    let lower = line.trim().trim_end_matches(':').to_lowercase();
    SECTION_HEADERS
        .iter()
        .any(|header| lower == *header || lower.starts_with(&format!("{header} ")))
}

pub fn is_bad_name(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    lower.is_empty() || lower == "candidate" || is_section_header(name)
}

fn looks_like_name(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.len() > 60 || line.contains('@') || line.contains("http") {
        return false;
    }
    if is_section_header(line) {
        return false;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    (2..=4).contains(&words.len())
        && words.iter().all(|w| {
            w.chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'' || c == '.')
        })
        && !line.to_lowercase().contains("resume")
        && !line.to_lowercase().contains("curriculum")
}

fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let rest = chars.as_str().to_lowercase();
                    format!("{}{}", first.to_uppercase(), rest)
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Best-effort name from the top lines, falling back to the file stem.
pub fn derive_name(file_name: &str, text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if let Some(line) = lines
        .iter()
        .take(12)
        .find(|line| looks_like_name(line) && !is_section_header(line))
    {
        return normalize_name(line);
    }

    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let words: Vec<&str> = stem
        .split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|word| !word.is_empty())
        .filter(|word| {
            let lower = word.to_lowercase();
            !matches!(
                lower.as_str(),
                "resume" | "cv" | "final" | "updated" | "new" | "copy" | "pdf" | "docx"
            ) && !word.chars().all(|c| c.is_ascii_digit())
        })
        .collect();
    if words.is_empty() {
        "Candidate".into()
    } else {
        normalize_name(&words.join(" "))
    }
}

pub fn parse_resume(path: &Path) -> Result<Candidate> {
    let raw = extract_text(path)?;
    let text = clean_text(&raw);
    if text.split_whitespace().count() < 20 {
        return Err(anyhow!(
            "No usable text found. This looks like a scanned PDF; OCR is not enabled yet."
        ));
    }
    Ok(profile_from_text(path, text))
}

pub fn profile_from_text(path: &Path, text: String) -> Candidate {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("resume")
        .to_string();

    let email_re = Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap();
    let phone_re = Regex::new(r"(\+?\d[\d\s().\-]{7,}\d)").unwrap();

    let email = email_re
        .find(&text)
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let phone = phone_re
        .find(&text)
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default();

    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();

    let name = derive_name(&file_name, &text);

    let role_words = [
        "engineer",
        "developer",
        "designer",
        "manager",
        "scientist",
        "analyst",
        "architect",
        "consultant",
        "director",
        "lead",
        "specialist",
        "recruiter",
        "product",
        "data",
        "security",
        "devops",
        "platform",
    ];
    let headline = lines
        .iter()
        .skip_while(|l| **l == name)
        .find(|l| {
            let lower = l.to_lowercase();
            l.len() < 120 && role_words.iter().any(|w| lower.contains(w))
        })
        .map(|l| l.to_string())
        .unwrap_or_else(|| "Candidate".into());

    let lower_text = text.to_lowercase();
    let skills: Vec<String> = SKILLS
        .iter()
        .filter(|skill| {
            let s = skill.to_lowercase();
            if s.len() <= 3 {
                lower_text
                    .split(|c: char| !c.is_alphanumeric() && c != '+' && c != '#')
                    .any(|w| w == s)
            } else {
                lower_text.contains(&s)
            }
        })
        .map(|s| s.to_string())
        .collect();

    let years_re = Regex::new(r"(?i)(\d{1,2})\s*\+?\s*(?:years|yrs)").unwrap();
    let years = years_re
        .captures_iter(&text)
        .filter_map(|c| c.get(1)?.as_str().parse::<f32>().ok())
        .fold(0.0f32, f32::max);

    let mut summary = text
        .lines()
        .skip_while(|l| *l == name)
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    if summary.chars().count() > 340 {
        summary = summary.chars().take(340).collect::<String>();
        summary.push('…');
    }

    let location = lines
        .iter()
        .find(|l| {
            let lower = l.to_lowercase();
            (lower.contains(',') && l.len() < 80)
                || lower.contains("remote")
                || lower.contains("based in")
        })
        .map(|l| l.to_string())
        .unwrap_or_default();

    Candidate {
        id: Uuid::new_v4().to_string(),
        name: name.trim().to_string(),
        headline,
        location,
        email,
        phone,
        years,
        skills,
        summary,
        text,
        file_name,
        created_at: now_ms(),
        is_demo: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_section_headers_and_uses_file_stem() {
        let text =
            "ALOK MISHRA / heyalok.com\n\nhey.alokmishra@gmail.com\n\nTECHNICAL SKILLS\nRust\n";
        assert_eq!(derive_name("Alok_Mishra_Resume.pdf", text), "Alok Mishra");
        assert!(is_bad_name("TECHNICAL SKILLS"));
        assert!(!is_bad_name("Alok Mishra"));
    }

    #[test]
    fn finds_a_name_above_sections() {
        let text = "TECHNICAL SKILLS\n\nAlok Mishra\n\nExperience\n";
        assert_eq!(derive_name("resume.pdf", text), "Alok Mishra");
    }
}
