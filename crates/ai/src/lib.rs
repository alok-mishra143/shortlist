use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use shortlist_core::{ChatMessage, ModelRef, now_ms};
use std::io::{BufRead, BufReader};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy)]
pub struct OpenCode;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExtractedProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub headline: String,
    #[serde(default)]
    pub location: String,
    #[serde(default, deserialize_with = "deserialize_optional_f32")]
    pub years: Option<f32>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub phone: String,
}

fn deserialize_optional_f32<'de, D>(deserializer: D) -> std::result::Result<Option<f32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Number(number)) => number.as_f64().map(|v| v as f32),
        Some(serde_json::Value::String(value)) => value.trim().parse::<f32>().ok(),
        _ => None,
    })
}

#[derive(Clone, Debug, Default)]
pub struct StreamState {
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelRow {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelRow>,
}

impl OpenCode {
    fn run(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut cmd = Command::new("opencode");
        cmd.arg("api").arg(method).arg(path);
        if let Some(body) = body {
            cmd.arg("-d").arg(serde_json::to_string(body)?);
        }
        if let Some(home) = directories_home() {
            cmd.current_dir(home);
        }
        let output = cmd.output().context(
            "Could not run `opencode`. Install OpenCode and sign in in your terminal first.",
        )?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            let msg = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                format!("opencode api exited with {}", output.status)
            };
            return Err(anyhow!(msg));
        }
        let start = stdout.find('{').unwrap_or(0);
        let end = stdout.rfind('}').map(|i| i + 1).unwrap_or(stdout.len());
        serde_json::from_str::<Value>(&stdout[start..end])
            .with_context(|| format!("unexpected OpenCode response: {}", stdout.trim()))
    }

    pub fn info(&self) -> Result<Value> {
        self.run("get", "/api/info", None)
    }

    pub fn is_available(&self) -> bool {
        Command::new("opencode")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn version(&self) -> String {
        Command::new("opencode")
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "not found".into())
    }

    pub fn models(&self) -> Result<Vec<ModelRef>> {
        let value = self.run("get", "/api/model", None)?;
        let parsed: ModelsResponse = serde_json::from_value(value).context("parse model list")?;
        let mut out = Vec::new();
        for row in parsed.data {
            if row.status != "active" || row.enabled == Some(false) {
                continue;
            }
            if out
                .iter()
                .any(|m: &ModelRef| m.id == row.id && m.provider_id == row.provider_id)
            {
                continue;
            }
            out.push(ModelRef {
                name: if row.name.is_empty() {
                    row.id.clone()
                } else {
                    row.name
                },
                id: row.id,
                provider_id: row.provider_id,
            });
        }
        out.sort_by(|a, b| {
            (a.provider_id.as_str(), a.name.as_str())
                .cmp(&(b.provider_id.as_str(), b.name.as_str()))
        });
        Ok(out)
    }

    pub fn create_session(&self, title: &str, model: Option<&ModelRef>) -> Result<String> {
        let mut body = json!({ "title": title });
        if let Some(model) = model {
            body["model"] = json!({ "id": model.id, "providerID": model.provider_id });
        }
        let value = self.run("post", "/api/session", Some(&body))?;
        value
            .pointer("/data/id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("OpenCode did not return a session id"))
    }

    pub fn delete_session(&self, session_id: &str) -> Result<()> {
        self.run("delete", &format!("/api/session/{session_id}"), None)?;
        Ok(())
    }

    pub fn set_model(&self, session_id: &str, model: &ModelRef) -> Result<()> {
        let body = json!({ "model": { "id": model.id, "providerID": model.provider_id } });
        self.run(
            "post",
            &format!("/api/session/{session_id}/model"),
            Some(&body),
        )?;
        Ok(())
    }

    pub fn prompt(&self, session_id: &str, text: &str) -> Result<()> {
        let body = json!({ "text": text });
        self.run(
            "post",
            &format!("/api/session/{session_id}/prompt"),
            Some(&body),
        )?;
        Ok(())
    }

    /// Ask the selected model to turn resume text into a structured profile.
    pub fn extract_profile(
        &self,
        model: Option<&ModelRef>,
        text: &str,
    ) -> Result<ExtractedProfile> {
        let truncated: String = text.chars().take(14000).collect();
        let prompt = String::from(
            r#"You extract structured candidate details from a resume. Return ONLY a JSON object, with no markdown fences and no commentary. Use only facts present in the resume. Use an empty string or empty array when a fact is missing.
Schema: {"name": string, "headline": string, "location": string, "years": number or null, "skills": [string], "summary": string, "email": string, "phone": string}
Rules: name is the person's full name, never a section heading. headline is their current or most recent job title. location is city and country. years is total professional experience as a number. skills are concrete technologies and tools, at most 20. summary is one or two sentences.

RESUME:
"#,
        ) + &truncated;
        let session = self.create_session("Shortlist profile extraction", model)?;
        let after = now_ms();
        let result = self
            .prompt(&session, &prompt)
            .and_then(|_| self.wait_for_reply(&session, after, Duration::from_secs(120)));
        let _ = self.delete_session(&session);
        let reply = result?;
        let json = extract_json_object(&reply)
            .ok_or_else(|| anyhow!("The model did not return a JSON object."))?;
        serde_json::from_str(&json).map_err(|error| anyhow!("Invalid extraction JSON: {error}"))
    }

    /// Stream a prompt through the local OpenCode HTTP API.
    ///
    /// The service's SSE bus emits `session.text.delta` events. We connect
    /// before sending the prompt, accumulate deltas into `state`, and return
    /// when that session's execution finishes.
    pub fn stream_prompt(
        &self,
        session_id: &str,
        text: &str,
        state: Arc<Mutex<StreamState>>,
    ) -> Result<String> {
        let url = service_url().ok_or_else(|| {
            anyhow!("OpenCode service is not running. Open OpenCode in your terminal once.")
        })?;
        let password = service_password()
            .ok_or_else(|| anyhow!("Could not read the local OpenCode service password."))?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()?;
        let response = client
            .get(format!("{url}/api/event"))
            .basic_auth(service_username(), Some(password.clone()))
            .send()?
            .error_for_status()?;
        let mut reader = BufReader::new(response);
        let mut line = String::new();
        let mut sent = false;
        let mut accumulated = String::new();

        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let Some(payload) = line.trim().strip_prefix("data: ") else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let kind = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            if kind == "server.connected" && !sent {
                sent = true;
                let response = client
                    .post(format!("{url}/api/session/{session_id}/prompt"))
                    .basic_auth(service_username(), Some(password.clone()))
                    .json(&json!({ "text": text }))
                    .send()?;
                if !response.status().is_success() {
                    let body = response.text().unwrap_or_default();
                    return Err(anyhow!("OpenCode rejected the prompt: {body}"));
                }
                continue;
            }
            if !sent {
                continue;
            }

            let data = event.get("data").cloned().unwrap_or(Value::Null);
            let event_session = data.get("sessionID").and_then(|v| v.as_str()).unwrap_or("");
            if !event_session.is_empty() && event_session != session_id {
                continue;
            }

            match kind {
                "session.text.delta" => {
                    if let Some(delta) = data.get("delta").and_then(|v| v.as_str()) {
                        accumulated.push_str(delta);
                        let mut snapshot = state.lock().unwrap();
                        snapshot.text = accumulated.clone();
                        snapshot.done = false;
                    }
                }
                "session.execution.succeeded" => {
                    let mut snapshot = state.lock().unwrap();
                    snapshot.text = accumulated.clone();
                    snapshot.done = true;
                    return Ok(accumulated);
                }
                "session.execution.failed" => {
                    let message = data
                        .get("error")
                        .and_then(|err| err.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("The model call failed.");
                    let mut snapshot = state.lock().unwrap();
                    snapshot.done = true;
                    snapshot.error = Some(message.to_string());
                    return Err(anyhow!(message.to_string()));
                }
                _ => {}
            }
        }

        let mut snapshot = state.lock().unwrap();
        snapshot.text = accumulated.clone();
        snapshot.done = true;
        if accumulated.is_empty() {
            Err(anyhow!(
                "The model finished without returning text. Check the selected model."
            ))
        } else {
            Ok(accumulated)
        }
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let value = self.run("get", &format!("/api/session/{session_id}/message"), None)?;
        let data = value
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for item in data {
            let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let id = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let created = item
                .pointer("/time/created")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            match kind {
                "user" => {
                    let text = item
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    out.push(ChatMessage {
                        id,
                        session_id: session_id.to_string(),
                        role: "user".into(),
                        text,
                        created_at: created,
                    });
                }
                "assistant" => {
                    let mut text = String::new();
                    if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                        for part in content {
                            if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        text.push('\n');
                                    }
                                    text.push_str(t);
                                }
                            }
                        }
                    }
                    if text.is_empty() {
                        if let Some(err) = item.pointer("/error/message").and_then(|v| v.as_str()) {
                            text = format!("⚠ {err}");
                        }
                    }
                    if !text.is_empty() {
                        out.push(ChatMessage {
                            id,
                            session_id: session_id.to_string(),
                            role: "assistant".into(),
                            text,
                            created_at: created,
                        });
                    }
                }
                _ => {}
            }
        }
        out.sort_by_key(|m| m.created_at);
        Ok(out)
    }

    /// Poll until the assistant reply for this turn is complete.
    pub fn wait_for_reply(
        &self,
        session_id: &str,
        after_ms: i64,
        timeout: Duration,
    ) -> Result<String> {
        let started = now_ms();
        loop {
            if now_ms() - started > timeout.as_millis() as i64 {
                return Err(anyhow!("Timed out waiting for the model."));
            }
            let value = self.run("get", &format!("/api/session/{session_id}/message"), None)?;
            let data = value
                .get("data")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for item in &data {
                if item.get("type").and_then(|v| v.as_str()) != Some("assistant") {
                    continue;
                }
                let created = item
                    .pointer("/time/created")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if created < after_ms {
                    continue;
                }
                let finish = item.get("finish").and_then(|v| v.as_str());
                if finish.is_none() {
                    continue;
                }
                let mut text = String::new();
                if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
                    for part in content {
                        if part.get("type").and_then(|v| v.as_str()) == Some("text") {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                    }
                }
                if finish == Some("error") {
                    let err = item
                        .pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("The model returned an error.");
                    return Err(anyhow!(err.to_string()));
                }
                if !text.is_empty() {
                    return Ok(text);
                }
                return Ok("(The model returned an empty response.)".into());
            }
            for item in &data {
                if item.get("type").and_then(|v| v.as_str()) != Some("idle") {
                    continue;
                }
                let created = item
                    .pointer("/time/created")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                if created < after_ms {
                    continue;
                }
                if item.get("outcome").and_then(|v| v.as_str()) == Some("failed") {
                    let err = item
                        .pointer("/error/message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("The model call failed.");
                    return Err(anyhow!(err.to_string()));
                }
            }
            std::thread::sleep(Duration::from_millis(600));
        }
    }
}

fn directories_home() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

fn service_url() -> Option<String> {
    let output = Command::new("opencode")
        .args(["service", "status"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(|line| line.trim_end_matches('/').to_string())
}

fn service_username() -> String {
    std::env::var("OPENCODE_SERVER_USERNAME").unwrap_or_else(|_| "opencode".into())
}

fn service_password() -> Option<String> {
    if let Ok(password) = std::env::var("OPENCODE_SERVER_PASSWORD") {
        if !password.is_empty() {
            return Some(password);
        }
    }
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    for path in [
        home.join(".config/opencode/service.json"),
        home.join(".local/share/opencode/service.json"),
    ] {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<Value>(&data) {
                if let Some(password) = value.get("password").and_then(|v| v.as_str()) {
                    return Some(password.to_string());
                }
            }
        }
    }
    None
}

fn extract_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| text[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_json_from_a_fenced_reply() {
        let reply = "```json\n{\"name\":\"Alok Mishra\",\"years\":\"4\"}\n```";
        let json = extract_json_object(reply).unwrap();
        let profile: ExtractedProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(profile.name, "Alok Mishra");
        assert_eq!(profile.years, Some(4.0));
    }
}
