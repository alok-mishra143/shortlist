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

const EXPERIENCE_HEADERS: &[&str] = &[
    "experience",
    "work experience",
    "professional experience",
    "employment",
    "employment history",
    "work history",
    "career history",
];

const SECTION_STOPS: &[&str] = &[
    "education",
    "skills",
    "technical skills",
    "projects",
    "certifications",
    "awards",
    "publications",
    "languages",
    "volunteer",
    "interests",
    "summary",
    "profile",
    "references",
];

/// The text between a section heading and the next section heading.
fn section_body(text: &str, headers: &[&str]) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|line| {
        let clean = line.trim().trim_end_matches(':').to_lowercase();
        headers.contains(&clean.as_str())
    })?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| {
            let clean = line.trim().trim_end_matches(':').to_lowercase();
            SECTION_STOPS.contains(&clean.as_str())
        })
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    let body = lines[start + 1..end].join("\n");
    (!body.trim().is_empty()).then_some(body)
}

fn parse_month_year(value: &str) -> Option<(String, i32, usize)> {
    let re = Regex::new(
        r"(?i)((?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?\s*)?((?:19|20)\d{2})",
    )
    .ok()?;
    let caps = re.captures(value)?;
    let label = caps.get(0)?.as_str().trim().to_string();
    let year = caps.get(2)?.as_str().parse::<i32>().ok()?;
    let end = caps.get(0)?.end();
    Some((label, year, end))
}

fn date_range(line: &str) -> Option<(String, String, f32)> {
    let lower = line.to_lowercase();
    let looks_like_range = lower.contains("present")
        || lower.contains("current")
        || lower.contains("now")
        || lower.contains("–")
        || lower.contains("—")
        || lower.contains(" to ")
        || (lower.contains('-') && lower.matches(|c: char| c.is_ascii_digit()).count() >= 4);
    if !looks_like_range {
        return None;
    }
    let first = parse_month_year(line)?;
    let rest = &line[first.2.min(line.len())..];
    let end = if lower.contains("present") || lower.contains("current") || lower.contains("now") {
        "Present".to_string()
    } else {
        parse_month_year(rest)
            .map(|(label, _, _)| label)
            .unwrap_or_default()
    };
    if end.is_empty() && !lower.contains("present") {
        return None;
    }
    let end_year = if end == "Present" {
        current_year()
    } else {
        parse_month_year(&end)
            .map(|(_, year, _)| year)
            .unwrap_or(first.1)
    };
    let years = (end_year - first.1).max(0) as f32;
    Some((first.0, end, years))
}

fn current_year() -> i32 {
    1970 + (now_ms() / 31_556_952_000) as i32
}

fn role_word(value: &str) -> bool {
    let value = value.to_lowercase();
    [
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
        "intern",
        "founder",
        "president",
        "officer",
    ]
    .iter()
    .any(|word| value.contains(word))
}

fn title_and_company(previous: &[&str]) -> (String, String) {
    let mut lines = previous
        .iter()
        .rev()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with(['•', '-', '*', '·']));
    let Some(first) = lines.next() else {
        return (String::new(), String::new());
    };
    for separator in [" at ", " | ", " · ", " – ", " — ", ", "] {
        if let Some((left, right)) = first.split_once(separator) {
            let (left, right) = (left.trim(), right.trim());
            if !left.is_empty() && !right.is_empty() {
                return if role_word(right) && !role_word(left) {
                    (right.to_string(), left.to_string())
                } else {
                    (left.to_string(), right.to_string())
                };
            }
        }
    }
    let company = lines
        .next()
        .filter(|line| line.len() < 90)
        .map(|line| line.trim().to_string())
        .unwrap_or_default();
    (first.trim().to_string(), company)
}

fn extract_experience(text: &str) -> Vec<shortlist_core::Experience> {
    let Some(section) = section_body(text, EXPERIENCE_HEADERS) else {
        return Vec::new();
    };
    let lines: Vec<&str> = section.lines().collect();
    let mut roles = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((start, end, years)) = date_range(line) else {
            continue;
        };
        let (title, company) = title_and_company(&lines[..index]);
        if title.is_empty() {
            continue;
        }
        let mut description = String::new();
        for (next_index, next) in lines.iter().enumerate().skip(index + 1) {
            if date_range(next).is_some() {
                break;
            }
            // A short title/company line immediately before the next date
            // belongs to the next role, not to this description.
            let trimmed = next.trim();
            let bullet = trimmed.starts_with(['•', '-', '*', '·']);
            let next_role_header = !bullet
                && !trimmed.ends_with('.')
                && trimmed.split_whitespace().count() <= 12
                && lines
                    .iter()
                    .skip(next_index + 1)
                    .take(2)
                    .any(|line| date_range(line).is_some());
            if next_role_header {
                break;
            }
            let next = trimmed;
            if next.is_empty() {
                continue;
            }
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(next.trim_start_matches(['•', '-', '*', '·']).trim());
            if description.chars().count() > 420 {
                break;
            }
        }
        if description.chars().count() > 420 {
            description = description.chars().take(420).collect::<String>();
            description.push('…');
        }
        if roles.iter().any(|role: &shortlist_core::Experience| {
            role.title.eq_ignore_ascii_case(&title) && role.start.eq_ignore_ascii_case(&start)
        }) {
            continue;
        }
        roles.push(shortlist_core::Experience {
            title,
            company,
            start,
            end,
            years,
            description,
        });
        if roles.len() >= 10 {
            break;
        }
    }
    roles
}

fn extract_education(text: &str) -> Vec<shortlist_core::Education> {
    let scope = section_body(text, &["education", "academic background"])
        .unwrap_or_else(|| text.to_string());
    let degree_re = Regex::new(
        r"(?i)(b\.?tech|b\.?e\.?\b|b\.?s\.?\b|b\.?a\.?\b|m\.?tech|m\.?s\.?\b|m\.?a\.?\b|mba|ph\.?d|bachelor|master|doctorate|diploma|degree)",
    )
    .unwrap_or_else(|_| Regex::new("$^").expect("empty regex"));
    let institution_re = Regex::new(r"(?i)(university|college|institute|school|academy)").unwrap();
    let year_re = Regex::new(r"\b(19|20)\d{2}\b").unwrap();
    let lines: Vec<&str> = scope
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut education = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !degree_re.is_match(line) {
            continue;
        }
        let institution = lines[index + 1..]
            .iter()
            .take(2)
            .find(|candidate| institution_re.is_match(candidate))
            .map(|candidate| candidate.to_string())
            .unwrap_or_default();
        let year = year_re
            .find(line)
            .or_else(|| {
                lines[index + 1..]
                    .iter()
                    .take(2)
                    .find_map(|candidate| year_re.find(candidate))
            })
            .map(|found| found.as_str().to_string())
            .unwrap_or_default();
        education.push(shortlist_core::Education {
            degree: line.trim_start_matches(['•', '-', '*', '·']).to_string(),
            institution,
            year,
        });
        if education.len() >= 4 {
            break;
        }
    }
    education
}

fn extract_certifications(text: &str) -> Vec<String> {
    let Some(section) = section_body(
        text,
        &["certifications", "certificates", "licenses", "courses"],
    ) else {
        return Vec::new();
    };
    section
        .lines()
        .map(|line| line.trim().trim_start_matches(['•', '-', '*', '·']).trim())
        .filter(|line| line.len() >= 3 && line.len() <= 120)
        .take(10)
        .map(str::to_string)
        .collect()
}

/// Total calendar span covered by the roles, for filling in missing totals.
fn experience_span(experience: &[shortlist_core::Experience]) -> f32 {
    let mut earliest: Option<i32> = None;
    let mut latest: Option<i32> = None;
    for role in experience {
        if let Some((_, year, _)) = parse_month_year(&role.start) {
            earliest = Some(earliest.map_or(year, |value| value.min(year)));
        }
        let end_year = if role.is_current() {
            Some(current_year())
        } else {
            parse_month_year(&role.end).map(|(_, year, _)| year)
        };
        if let Some(year) = end_year {
            latest = Some(latest.map_or(year, |value| value.max(year)));
        }
    }
    match (earliest, latest) {
        (Some(start), Some(end)) => (end - start).max(0) as f32,
        _ => 0.0,
    }
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

    // Structured career data. The AI pass refines this; the heuristics mean an
    // import is still rankable when AI is unavailable.
    let experience = extract_experience(&text);
    let years = years.max(experience_span(&experience));
    let education = extract_education(&text);
    let certifications = extract_certifications(&text);

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

    let summary = section_body(&text, &["summary", "profile", "about"])
        .map(|body| {
            body.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(4)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|value| value.chars().count() >= 40)
        .unwrap_or_else(|| {
            lines
                .iter()
                .skip_while(|line| **line == name)
                .filter(|line| {
                    let lower = line.to_lowercase();
                    **line != headline
                        && **line != location
                        && !line.contains('@')
                        && !lower.contains("years of experience")
                        && !lower.contains("phone")
                        && !SECTION_STOPS.contains(&lower.as_str())
                        && (line.split_whitespace().count() >= 6 || line.ends_with('.'))
                })
                .take(3)
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        });
    let mut summary = summary;
    if summary.chars().count() > 340 {
        summary = summary.chars().take(340).collect::<String>();
        summary.push('…');
    }

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
        experience,
        education,
        certifications,
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

    #[test]
    fn extracts_roles_education_and_certifications() {
        let text = "\
Jane Doe
Senior Backend Engineer
London, UK

Experience
Stripe | Senior Backend Engineer
Mar 2021 – Present
Built payment systems and led the ledger service.

Acme | Software Engineer
2018 – 2021
Shipped internal tooling.

Education
B.Tech Computer Science
University of London
2014

Certifications
AWS Solutions Architect
Certified Kubernetes Administrator
";
        let candidate = profile_from_text(std::path::Path::new("jane_doe.pdf"), text.into());
        assert_eq!(candidate.experience.len(), 2, "{:?}", candidate.experience);
        assert_eq!(candidate.experience[0].title, "Senior Backend Engineer");
        assert_eq!(candidate.experience[0].company, "Stripe");
        assert!(candidate.experience[0].is_current());
        assert!(candidate.years >= 5.0, "{}", candidate.years);
        assert_eq!(candidate.education.len(), 1);
        assert_eq!(candidate.certifications.len(), 2);
        assert!(candidate.index_text().contains("Stripe"));
    }
}
