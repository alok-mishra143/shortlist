use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use shortlist_core::{Candidate, ChatMessage, ChatSession, now_ms};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

const NOTES_SECTION: &str = "\n\nRecruiter notes:\n";

pub struct Store {
    conn: Mutex<Connection>,
}

fn fts_query(raw: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "who", "what", "has", "have", "had", "did", "does", "this",
        "that", "they", "their", "about", "into", "from", "your", "you", "are", "was", "were",
        "will", "would", "can", "could", "should", "how", "why", "when", "which", "there", "here",
        "its", "of", "to", "in", "on", "at", "by", "as", "is", "be", "or", "if", "we", "me", "my",
        "our", "any", "all", "not", "but", "than",
    ];
    let terms: Vec<String> = raw
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .map(|t| format!("\"{}\"", t))
        .collect();
    if terms.is_empty() {
        "\"\"".into()
    } else {
        terms.join(" OR ")
    }
}

fn row_to_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<Candidate> {
    let skills_json: String = row.get("skills")?;
    let skills: Vec<String> = serde_json::from_str(&skills_json).unwrap_or_default();
    Ok(Candidate {
        id: row.get("id")?,
        name: row.get("name")?,
        headline: row.get("headline")?,
        location: row.get("location")?,
        email: row.get("email")?,
        phone: row.get("phone")?,
        years: row.get("years")?,
        skills,
        summary: row.get("summary")?,
        text: row.get("text")?,
        file_name: row.get("file_name")?,
        created_at: row.get("created_at")?,
        is_demo: row.get::<_, i64>("is_demo")? != 0,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).context("open sqlite")?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Mutex::new(Connection::open_in_memory()?),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

            CREATE TABLE IF NOT EXISTS candidates (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                headline TEXT NOT NULL DEFAULT '',
                location TEXT NOT NULL DEFAULT '',
                email TEXT NOT NULL DEFAULT '',
                phone TEXT NOT NULL DEFAULT '',
                years REAL NOT NULL DEFAULT 0,
                skills TEXT NOT NULL DEFAULT '[]',
                summary TEXT NOT NULL DEFAULT '',
                text TEXT NOT NULL DEFAULT '',
                file_name TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                is_demo INTEGER NOT NULL DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS candidate_fts USING fts5(
                candidate_id UNINDEXED,
                name,
                headline,
                skills,
                body,
                tokenize = 'porter unicode61'
            );

            CREATE TABLE IF NOT EXISTS chunks (
                id TEXT PRIMARY KEY,
                candidate_id TEXT NOT NULL,
                idx INTEGER NOT NULL,
                text TEXT NOT NULL,
                embedding BLOB,
                model TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS chunks_candidate ON chunks(candidate_id);

            CREATE TABLE IF NOT EXISTS candidate_notes (
                candidate_id TEXT PRIMARY KEY,
                body TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS chat_sessions (
                id TEXT PRIMARY KEY,
                opencode_id TEXT NOT NULL DEFAULT '',
                title TEXT NOT NULL DEFAULT 'New chat',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS chat_messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS messages_session ON chat_messages(session_id, created_at);

            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn count(&self) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row("SELECT COUNT(*) FROM candidates", [], |r| r.get(0))?)
    }

    pub fn insert_candidate(&self, c: &Candidate, chunks: &[String]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let skills = serde_json::to_string(&c.skills)?;
        tx.execute(
            "INSERT OR REPLACE INTO candidates
             (id, name, headline, location, email, phone, years, skills, summary, text, file_name, created_at, is_demo)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                c.id, c.name, c.headline, c.location, c.email, c.phone, c.years, skills,
                c.summary, c.text, c.file_name, c.created_at, c.is_demo as i64
            ],
        )?;
        let note: Option<String> = tx
            .query_row(
                "SELECT body FROM candidate_notes WHERE candidate_id = ?1",
                params![c.id],
                |r| r.get(0),
            )
            .optional()?;
        let body = match note {
            Some(note) => format!("{}{}{}", c.text, NOTES_SECTION, note),
            None => c.text.clone(),
        };
        tx.execute(
            "DELETE FROM candidate_fts WHERE candidate_id = ?1",
            params![c.id],
        )?;
        tx.execute(
            "INSERT INTO candidate_fts (candidate_id, name, headline, skills, body)
             VALUES (?1,?2,?3,?4,?5)",
            params![c.id, c.name, c.headline, skills, body],
        )?;
        tx.execute("DELETE FROM chunks WHERE candidate_id = ?1", params![c.id])?;
        for (idx, chunk) in chunks.iter().enumerate() {
            tx.execute(
                "INSERT INTO chunks (id, candidate_id, idx, text, embedding, model, created_at)
                 VALUES (?1,?2,?3,?4,NULL,'',?5)",
                params![
                    Uuid::new_v4().to_string(),
                    c.id,
                    idx as i64,
                    chunk,
                    now_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_candidates(&self, query: &str, limit: usize) -> Result<Vec<Candidate>> {
        let conn = self.conn.lock().unwrap();
        if query.trim().is_empty() {
            let mut stmt =
                conn.prepare("SELECT * FROM candidates ORDER BY created_at DESC LIMIT ?1")?;
            let rows = stmt.query_map(params![limit as i64], row_to_candidate)?;
            return Ok(rows.filter_map(|r| r.ok()).collect());
        }
        let mut stmt = conn.prepare(
            "SELECT c.* FROM candidate_fts f
             JOIN candidates c ON c.id = f.candidate_id
             WHERE candidate_fts MATCH ?1
             ORDER BY bm25(candidate_fts), c.created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![fts_query(query), limit as i64], row_to_candidate)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn candidate(&self, id: &str) -> Result<Option<Candidate>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT * FROM candidates WHERE id = ?1")?;
        Ok(stmt.query_row(params![id], row_to_candidate).optional()?)
    }

    pub fn update_candidate_name(&self, id: &str, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE candidates SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        conn.execute(
            "UPDATE candidate_fts SET name = ?1 WHERE candidate_id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    pub fn update_candidate(&self, candidate: &Candidate) -> Result<()> {
        let skills = serde_json::to_string(&candidate.skills)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE candidates SET name=?1, headline=?2, location=?3, email=?4, phone=?5,
             years=?6, skills=?7, summary=?8 WHERE id=?9",
            params![
                candidate.name,
                candidate.headline,
                candidate.location,
                candidate.email,
                candidate.phone,
                candidate.years,
                skills,
                candidate.summary,
                candidate.id
            ],
        )?;
        tx.execute(
            "UPDATE candidate_fts SET name=?1, headline=?2, skills=?3 WHERE candidate_id=?4",
            params![
                candidate.name,
                candidate.headline,
                serde_json::to_string(&candidate.skills)?,
                candidate.id
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_candidate(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM candidate_fts WHERE candidate_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM chunks WHERE candidate_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM candidate_notes WHERE candidate_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM candidates WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn note(&self, candidate_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT body FROM candidate_notes WHERE candidate_id = ?1")?;
        Ok(stmt
            .query_row(params![candidate_id], |r| r.get(0))
            .optional()?)
    }

    pub fn save_note(&self, candidate_id: &str, body: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO candidate_notes (candidate_id, body, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(candidate_id) DO UPDATE SET
             body = excluded.body, updated_at = excluded.updated_at",
            params![candidate_id, body, now_ms()],
        )?;
        tx.execute(
            "UPDATE candidate_fts
             SET body = (SELECT text FROM candidates WHERE id = ?1) || ?2
             WHERE candidate_id = ?1
               AND EXISTS (SELECT 1 FROM candidates WHERE id = ?1)",
            params![candidate_id, format!("{NOTES_SECTION}{body}")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete_note(&self, candidate_id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM candidate_notes WHERE candidate_id = ?1",
            params![candidate_id],
        )?;
        tx.execute(
            "UPDATE candidate_fts
             SET body = COALESCE((SELECT text FROM candidates WHERE id = ?1), '')
             WHERE candidate_id = ?1",
            params![candidate_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_all(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "DELETE FROM chat_messages; DELETE FROM chat_sessions; DELETE FROM chunks;
             DELETE FROM candidate_notes; DELETE FROM candidate_fts; DELETE FROM candidates;",
        )?;
        Ok(())
    }

    pub fn save_chunks_with_vectors(
        &self,
        candidate_id: &str,
        chunks: &[String],
        vectors: &[Vec<f32>],
        model: &str,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM chunks WHERE candidate_id = ?1",
            params![candidate_id],
        )?;
        for (idx, chunk) in chunks.iter().enumerate() {
            let blob = vectors.get(idx).map(|v| {
                let mut bytes = Vec::with_capacity(v.len() * 4);
                for f in v {
                    bytes.extend_from_slice(&f.to_le_bytes());
                }
                bytes
            });
            tx.execute(
                "INSERT INTO chunks (id, candidate_id, idx, text, embedding, model, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    Uuid::new_v4().to_string(),
                    candidate_id,
                    idx as i64,
                    chunk,
                    blob,
                    model,
                    now_ms()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn chunks_missing_vectors(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, candidate_id, text FROM chunks WHERE embedding IS NULL ORDER BY candidate_id, idx",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn set_chunk_vector(&self, chunk_id: &str, vector: &[f32], model: &str) -> Result<()> {
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chunks SET embedding = ?1, model = ?2 WHERE id = ?3",
            params![bytes, model, chunk_id],
        )?;
        Ok(())
    }

    pub fn embedded_chunks(&self) -> Result<Vec<(String, String, String, Vec<f32>)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT candidate_id, text, model, embedding FROM chunks WHERE embedding IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let cid: String = row.get(0)?;
            let text: String = row.get(1)?;
            let model: String = row.get(2)?;
            let blob: Vec<u8> = row.get(3)?;
            let mut vec = Vec::with_capacity(blob.len() / 4);
            for chunk in blob.chunks_exact(4) {
                vec.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            Ok((cid, text, model, vec))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn fts_candidates(&self, query: &str, limit: usize) -> Result<Vec<Candidate>> {
        self.list_candidates(query, limit)
    }

    pub fn create_session(&self, title: &str) -> Result<ChatSession> {
        let session = ChatSession {
            id: Uuid::new_v4().to_string(),
            opencode_id: String::new(),
            title: title.to_string(),
            created_at: now_ms(),
            updated_at: now_ms(),
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chat_sessions (id, opencode_id, title, created_at, updated_at)
             VALUES (?1,'',?2,?3,?4)",
            params![
                session.id,
                session.title,
                session.created_at,
                session.updated_at
            ],
        )?;
        Ok(session)
    }

    pub fn set_session_opencode_id(&self, id: &str, opencode_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chat_sessions SET opencode_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![opencode_id, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<ChatSession>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, opencode_id, title, created_at, updated_at
             FROM chat_sessions ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ChatSession {
                id: r.get(0)?,
                opencode_id: r.get(1)?,
                title: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn rename_session(&self, id: &str, title: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chat_sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn touch_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM chat_messages WHERE session_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM chat_sessions WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_message(&self, session_id: &str, role: &str, text: &str) -> Result<ChatMessage> {
        let message = ChatMessage {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            text: text.to_string(),
            created_at: now_ms(),
        };
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chat_messages (id, session_id, role, text, created_at)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                message.id,
                message.session_id,
                message.role,
                message.text,
                message.created_at
            ],
        )?;
        conn.execute(
            "UPDATE chat_sessions SET updated_at = ?1 WHERE id = ?2",
            params![message.created_at, session_id],
        )?;
        Ok(message)
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, text, created_at FROM chat_messages
             WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(ChatMessage {
                id: r.get(0)?,
                session_id: r.get(1)?,
                role: r.get(2)?,
                text: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        Ok(stmt.query_row(params![key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn seed_demo(&self) -> Result<usize> {
        let mut added = 0;
        for (idx, c) in demo_candidates().into_iter().enumerate() {
            if self.candidate(&c.id)?.is_some() {
                continue;
            }
            let chunks = shortlist_core::chunk_text(&c.text, 160, 24);
            self.insert_candidate(&c, &chunks)?;
            let _ = idx;
            added += 1;
        }
        Ok(added)
    }
}

pub fn file_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn demo_candidates() -> Vec<Candidate> {
    let now = now_ms();
    let rows: Vec<(&str, &str, &str, f32, &[&str], &str, &str)> = vec![
        (
            "Sarah Chen",
            "Senior Backend Engineer",
            "London, UK",
            8.0,
            &[
                "Rust",
                "Go",
                "PostgreSQL",
                "Kafka",
                "AWS",
                "Distributed Systems",
            ],
            "Backend engineer who built payment infrastructure at a fintech scale-up. Led a team of five, owned the ledger service, and cut p99 latency from 480 ms to 90 ms.",
            "sarah_chen_resume.pdf",
        ),
        (
            "Marcus Webb",
            "Staff Platform Engineer",
            "Berlin, Germany",
            11.0,
            &[
                "Kubernetes",
                "Terraform",
                "AWS",
                "Go",
                "Platform Engineering",
                "CI/CD",
            ],
            "Platform lead responsible for a 200-service Kubernetes estate. Built the internal developer platform and reduced deploy time from 40 minutes to 6.",
            "marcus_webb_cv.pdf",
        ),
        (
            "Priya Nair",
            "Product Designer",
            "Bengaluru, India",
            6.0,
            &[
                "Figma",
                "Design Systems",
                "Prototyping",
                "User Research",
                "UI/UX",
            ],
            "Product designer focused on complex B2B tools. Built the design system used by four product teams and led research for the admin console redesign.",
            "priya_nair_resume.docx",
        ),
        (
            "Daniel Okafor",
            "Data Scientist",
            "Remote, Nigeria",
            5.0,
            &["Python", "PyTorch", "NLP", "SQL", "Machine Learning", "LLM"],
            "Data scientist working on retrieval and ranking. Shipped a semantic search system that lifted recruiter response rates by 18%.",
            "daniel_okafor_resume.pdf",
        ),
        (
            "Elena Rossi",
            "Engineering Manager",
            "Milan, Italy",
            12.0,
            &["Leadership", "Rust", "Microservices", "Agile", "Hiring"],
            "Engineering manager for two teams building high-throughput APIs. Grew the org from 6 to 18 engineers and introduced a structured hiring loop.",
            "elena_rossi_cv.pdf",
        ),
        (
            "Tomas Silva",
            "Full Stack Engineer",
            "Lisbon, Portugal",
            4.0,
            &["TypeScript", "React", "Node.js", "PostgreSQL", "GraphQL"],
            "Full stack product engineer. Shipped the customer portal and the billing workflow, working across React and Node services.",
            "tomas_silva_resume.pdf",
        ),
        (
            "Aisha Rahman",
            "Machine Learning Engineer",
            "Toronto, Canada",
            7.0,
            &[
                "Python",
                "TensorFlow",
                "MLOps",
                "Kubernetes",
                "Deep Learning",
            ],
            "ML engineer who took a recommendation model from notebook to production, serving 40M requests per day with sub-50 ms latency.",
            "aisha_rahman_resume.pdf",
        ),
        (
            "Jonas Berg",
            "DevOps Engineer",
            "Stockholm, Sweden",
            9.0,
            &[
                "AWS",
                "Terraform",
                "Docker",
                "Linux",
                "Observability",
                "CI/CD",
            ],
            "DevOps engineer specialising in observability and cost. Cut cloud spend by 31% while improving on-call signal-to-noise.",
            "jonas_berg_cv.docx",
        ),
        (
            "Mei Lin",
            "Frontend Engineer",
            "Singapore",
            5.0,
            &["React", "TypeScript", "Next.js", "Accessibility", "CSS"],
            "Frontend engineer with a strong accessibility practice. Rebuilt the checkout flow and raised conversion by 12%.",
            "mei_lin_resume.pdf",
        ),
        (
            "Oliver Grant",
            "Product Manager",
            "New York, USA",
            10.0,
            &[
                "Product Management",
                "Analytics",
                "SQL",
                "Roadmapping",
                "B2B",
            ],
            "Product manager for a recruiting platform. Launched AI-assisted screening and grew activated accounts 2.4x in two quarters.",
            "oliver_grant_resume.pdf",
        ),
        (
            "Fatima Zahra",
            "Security Engineer",
            "Paris, France",
            6.0,
            &["Security", "Rust", "Cryptography", "Threat Modeling", "AWS"],
            "Application security engineer. Led the migration to short-lived credentials and built the threat-modelling programme.",
            "fatima_zahra_resume.pdf",
        ),
        (
            "Noah Kim",
            "Data Engineer",
            "Seoul, South Korea",
            4.0,
            &["Python", "Spark", "Airflow", "dbt", "SQL", "Snowflake"],
            "Analytics engineer building the warehouse layer. Owns 120 dbt models and the ingestion pipelines behind the executive dashboard.",
            "noah_kim_resume.pdf",
        ),
    ];

    rows.into_iter()
        .enumerate()
        .map(|(i, (name, headline, location, years, skills, summary, file))| Candidate {
            id: format!("demo-{:02}", i + 1),
            name: name.into(),
            headline: headline.into(),
            location: location.into(),
            email: format!(
                "{}@example.com",
                name.to_lowercase().replace(' ', ".")
            ),
            phone: String::new(),
            years,
            skills: skills.iter().map(|s| s.to_string()).collect(),
            summary: summary.into(),
            text: format!(
                "{name}\n{headline}\n{location}\n\nSummary\n{summary}\n\nSkills\n{}\n\nExperience\n{headline} with {years} years of experience. Worked on production systems, partnered with product and design, and owned delivery end to end.",
                skills.join(", ")
            ),
            file_name: file.into(),
            created_at: now - (i as i64 * 86_400_000),
            is_demo: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_candidate_removes_fts_and_chunks() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        let mut all = store.list_candidates("", 1).unwrap();
        let candidate = all.remove(0);
        store.delete_candidate(&candidate.id).unwrap();
        assert!(store.candidate(&candidate.id).unwrap().is_none());
        assert!(
            store
                .list_candidates(&candidate.name, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.count().unwrap(), 11);
    }

    #[test]
    fn update_candidate_refreshes_fts() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        let mut all = store.list_candidates("", 1).unwrap();
        let mut candidate = all.remove(0);
        candidate.name = "Alok Mishra".into();
        candidate.headline = "Full-Stack Developer".into();
        candidate.skills = vec!["React".into(), "Node.js".into()];
        store.update_candidate(&candidate).unwrap();
        let loaded = store.candidate(&candidate.id).unwrap().unwrap();
        assert_eq!(loaded.name, "Alok Mishra");
        assert_eq!(loaded.headline, "Full-Stack Developer");
        assert!(!store.list_candidates("Alok", 10).unwrap().is_empty());
    }

    #[test]
    fn note_save_read_overwrite_and_delete() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        let id = "demo-01";
        assert!(store.note(id).unwrap().is_none());
        store.save_note(id, "Strong Rust background").unwrap();
        assert_eq!(
            store.note(id).unwrap().as_deref(),
            Some("Strong Rust background")
        );
        store.save_note(id, "Follow up about compensation").unwrap();
        assert_eq!(
            store.note(id).unwrap().as_deref(),
            Some("Follow up about compensation")
        );
        store.delete_note(id).unwrap();
        assert!(store.note(id).unwrap().is_none());
    }

    #[test]
    fn notes_are_searchable() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        let mut all = store.list_candidates("", 1).unwrap();
        let candidate = all.remove(0);
        store
            .save_note(&candidate.id, "zebracorn kubernetes operators")
            .unwrap();
        let found = store.list_candidates("zebracorn", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, candidate.id);
        store.delete_note(&candidate.id).unwrap();
        assert!(
            store
                .list_candidates("zebracorn", 10)
                .unwrap()
                .iter()
                .all(|c| c.id != candidate.id)
        );
    }

    #[test]
    fn delete_candidate_removes_note() {
        let store = Store::open_in_memory().unwrap();
        store.seed_demo().unwrap();
        let mut all = store.list_candidates("", 1).unwrap();
        let candidate = all.remove(0);
        store
            .save_note(&candidate.id, "Reached out on Monday")
            .unwrap();
        assert!(store.note(&candidate.id).unwrap().is_some());
        store.delete_candidate(&candidate.id).unwrap();
        assert!(store.note(&candidate.id).unwrap().is_none());
    }

    #[test]
    fn seed_search_and_chat() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.seed_demo().unwrap(), 12);
        let found = store.list_candidates("rust", 10).unwrap();
        assert!(found.iter().any(|c| c.name == "Sarah Chen"));
        let session = store.create_session("Test").unwrap();
        store.add_message(&session.id, "user", "hello").unwrap();
        assert_eq!(store.messages(&session.id).unwrap().len(), 1);
        store.delete_session(&session.id).unwrap();
        assert!(store.list_sessions().unwrap().is_empty());
    }
}
