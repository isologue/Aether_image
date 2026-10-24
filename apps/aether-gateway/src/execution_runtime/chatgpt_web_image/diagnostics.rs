use std::collections::VecDeque;
use std::sync::LazyLock;

use regex::Regex;

use super::*;
use crate::execution_runtime::transport::BufferedBodyReadDiagnostics;

const EVENT_TAIL_LIMIT: usize = 8;
const DIAGNOSTIC_TAG_LIMIT: usize = 80;
const TEXT_PREFIX_BYTES: usize = 2048;
const CONTENT_PART_LIMIT: usize = 16;
const PATCH_NODE_LIMIT: usize = 64;
const IMAGE_SCAN_NODE_LIMIT: usize = 4096;
const IMAGE_SCAN_DEPTH_LIMIT: usize = 24;

static PRIVATE_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (?:https?://|sediment://|file-service://|sandbox:/|data:image/)[^\s<>\"']+
        |\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b
        |\b(?:sk-|sess-|file-)[A-Z0-9_-]+
        |[A-Z0-9_+/=-]{40,}"#,
    )
    .expect("image diagnostic redaction regex should compile")
});

#[derive(Debug, Clone, Default)]
pub(super) struct WebImageSseDiagnostics {
    pub(super) response_bytes: usize,
    pub(super) event_count: u64,
    pub(super) json_event_count: u64,
    pub(super) json_error_count: u64,
    pub(super) done_marker_count: u64,
    pub(super) comment_line_count: u64,
    pub(super) patch_event_count: u64,
    pub(super) utf8_lossy: bool,
    pub(super) trailing_event_without_separator: bool,
    event_tail: VecDeque<EventDiagnostic>,
    last_message: MessageDiagnostic,
    last_assistant: Option<MessageDiagnostic>,
    image_hints: ImageHints,
    terminal_reason: Option<&'static str>,
    url_resolution_attempts: u64,
    resolved_url_observations: u64,
    download_attempts: u64,
    download_failures: u64,
    last_json_error: Option<JsonErrorDiagnostic>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct EventDiagnostic {
    event: Option<String>,
    event_type: Option<String>,
    shape: &'static str,
}

#[derive(Debug, Clone, Default)]
struct MessageDiagnostic {
    id: Option<String>,
    anchored: bool,
    source: Option<&'static str>,
    role: Option<String>,
    recipient: Option<String>,
    channel: Option<String>,
    status: Option<String>,
    end_turn: Option<bool>,
    content_type: Option<String>,
    part_types: Vec<String>,
    single_text_part: bool,
    text: TextPrefix,
}

// Never expose the retained prefix through derived Debug output. Only the
// redacted excerpt is emitted, and only for an attempt without an image.
#[derive(Clone)]
struct TextPrefix {
    value: String,
    truncated: bool,
    // Returned to the caller only for a confirmed terminal refusal. Logs keep
    // using the independently bounded, redacted prefix above.
    complete: Option<String>,
}

impl Default for TextPrefix {
    fn default() -> Self {
        Self {
            value: String::new(),
            truncated: false,
            complete: Some(String::new()),
        }
    }
}

impl std::fmt::Debug for TextPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextPrefix")
            .field("bytes", &self.value.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl TextPrefix {
    fn append(&mut self, text: &str) {
        if let Some(complete) = self.complete.as_mut() {
            if complete.len().saturating_add(text.len()) <= CHATGPT_WEB_IMAGE_SUMMARY_MAX_TEXT_BYTES
            {
                complete.push_str(text);
            } else {
                self.complete = None;
            }
        }
        let remaining = TEXT_PREFIX_BYTES.saturating_sub(self.value.len());
        let mut end = remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.value.push_str(&text[..end]);
        self.truncated |= end < text.len();
    }

    fn excerpt(&self) -> Option<String> {
        let clean: String = self
            .value
            .chars()
            .map(|ch| {
                if ch.is_control()
                    || matches!(ch, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                {
                    ' '
                } else {
                    ch
                }
            })
            .collect();
        let clean = PRIVATE_TEXT.replace_all(&clean, "[REDACTED]");
        let clean = crate::error::redact_error_str(&clean);
        let clean = clean.trim();
        (!clean.is_empty()).then(|| clean.to_string())
    }
}

#[derive(Debug, Clone, Default)]
struct ImageHints {
    nodes: usize,
    truncated: bool,
    image_structure_observations: u64,
    file_reference_observations: u64,
    sediment_reference_observations: u64,
    image_url_observations: u64,
    data_image_observations: u64,
    encoded_result_observations: u64,
    embedded_reference_observations: u64,
}

#[derive(Debug, Clone)]
struct JsonErrorDiagnostic {
    category: &'static str,
    line: usize,
    column: usize,
}

pub(super) fn diagnostic_event_tag(value: &str) -> String {
    if value.len() <= DIAGNOSTIC_TAG_LIMIT
        && !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        value.to_string()
    } else {
        "<nonstandard>".to_string()
    }
}

fn channel_tag(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(value) => Some(string_tag(Some(value)).unwrap_or_else(|| "<invalid>".to_string())),
    }
}

fn string_tag(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(diagnostic_event_tag)
}

impl WebImageSseDiagnostics {
    fn push_event(
        &mut self,
        event: Option<String>,
        event_type: Option<String>,
        shape: &'static str,
    ) {
        self.event_count = self.event_count.saturating_add(1);
        if self.event_tail.len() == EVENT_TAIL_LIMIT {
            self.event_tail.pop_front();
        }
        self.event_tail.push_back(EventDiagnostic {
            event,
            event_type,
            shape,
        });
    }

    pub(super) fn observe_done(&mut self, event: Option<String>) {
        self.done_marker_count = self.done_marker_count.saturating_add(1);
        self.push_event(event, None, "done");
    }

    pub(super) fn observe_json_error(&mut self, event: Option<String>, error: &serde_json::Error) {
        self.json_error_count = self.json_error_count.saturating_add(1);
        self.last_json_error = Some(JsonErrorDiagnostic {
            category: match error.classify() {
                serde_json::error::Category::Io => "io",
                serde_json::error::Category::Syntax => "syntax",
                serde_json::error::Category::Data => "data",
                serde_json::error::Category::Eof => "eof",
            },
            line: error.line(),
            column: error.column(),
        });
        self.push_event(event, None, "invalid_json");
    }

    pub(super) fn observe_json(&mut self, event: Option<String>, value: &Value) {
        self.json_event_count = self.json_event_count.saturating_add(1);
        let payload = value
            .get("v")
            .filter(|value| value.is_object())
            .unwrap_or(value);
        let event_type = string_tag(value.get("type")).or_else(|| string_tag(payload.get("type")));
        let is_patch = value.get("p").is_some()
            || value.get("o").is_some()
            || value.get("v").is_some_and(Value::is_array);
        if is_patch {
            self.patch_event_count = self.patch_event_count.saturating_add(1);
        }
        // Conversation polls contain historical input and output. Count only
        // the active message, matching the terminal-state boundary below.
        let evidence = if payload.get("mapping").is_some() {
            current_message(value)
        } else {
            Some(value)
        };
        if let Some(evidence) = evidence {
            self.image_hints.observe(evidence, None, 0);
        }
        let shape = if current_message(value).is_some() {
            if value.get("v").is_some() {
                "v1_message"
            } else {
                "message"
            }
        } else if is_patch {
            "patch"
        } else if value.is_object() {
            "object"
        } else {
            "non_object"
        };
        self.push_event(event, event_type, shape);
    }

    pub(super) fn observe_progress(&mut self, value: &Value) -> Option<bool> {
        let payload = value
            .get("v")
            .filter(|value| value.is_object())
            .unwrap_or(value);
        let mut remaining = PATCH_NODE_LIMIT;
        let changed = if let Some(message) = current_message(value) {
            let source = if payload.get("mapping").is_some() {
                "poll_current_node"
            } else {
                "snapshot"
            };
            self.last_message = MessageDiagnostic::from_message(message, source);
            true
        } else if payload.get("mapping").is_some() {
            // An unavailable current node cannot validate an older terminal.
            self.last_message = MessageDiagnostic {
                source: Some("poll_missing_current"),
                ..MessageDiagnostic::default()
            };
            true
        } else {
            self.observe_patch(value, "", 0, &mut remaining)
        };
        if !changed {
            return None;
        }
        if self.last_message.role.as_deref() == Some("assistant") {
            self.last_assistant = Some(self.last_message.clone());
        }
        let finished = self.last_message.is_terminal();
        self.terminal_reason = finished.then_some("assistant_end_turn");
        Some(finished)
    }

    pub(super) fn observe_response_completed(&mut self) {
        self.terminal_reason = Some("response_completed");
    }

    // Apply only the bounded metadata and text needed here, never a full SSE
    // document. Relative paths in nested patches are rooted at their parent.
    fn observe_patch(
        &mut self,
        value: &Value,
        base: &str,
        depth: usize,
        remaining: &mut usize,
    ) -> bool {
        if depth >= 4 || *remaining == 0 {
            self.last_message.anchored = false;
            return true;
        }
        *remaining -= 1;
        let raw_path = value.get("p").and_then(Value::as_str).unwrap_or("");
        if raw_path.len() > 128 {
            self.last_message.anchored = false;
            return true;
        }
        let path = if base.is_empty() || raw_path.starts_with("/message") {
            raw_path.to_string()
        } else {
            format!("{base}{raw_path}")
        };
        let operation = value.get("o").and_then(Value::as_str);
        let patch_value = value.get("v");
        if operation == Some("patch")
            || (raw_path.is_empty() && patch_value.is_some_and(Value::is_array))
        {
            let Some(patches) = patch_value.and_then(Value::as_array) else {
                return false;
            };
            let mut changed = false;
            for patch in patches.iter().take(16) {
                changed |= self.observe_patch(patch, &path, depth + 1, remaining);
                if *remaining == 0 {
                    self.last_message.anchored = false;
                    changed = true;
                    break;
                }
            }
            if patches.len() > 16 {
                self.last_message.anchored = false;
                changed = true;
            }
            return changed;
        }
        if !path.starts_with("/message") {
            return false;
        }
        let replace = matches!(operation, Some("add" | "replace" | "remove"));
        let patch_value = if operation == Some("remove") {
            None
        } else {
            patch_value
        };
        if !replace && operation != Some("append") {
            return false;
        }
        // A new message identity must never inherit the previous message's
        // role/end_turn/status. Require a fresh full snapshot as the anchor.
        if path == "/message/id" {
            let id = patch_value
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 256);
            if id != self.last_message.id.as_deref() {
                self.last_message = MessageDiagnostic {
                    id: id.map(str::to_owned),
                    ..MessageDiagnostic::default()
                };
            }
            return true;
        }
        if path == "/message" && patch_value.is_none() {
            self.last_message = MessageDiagnostic::default();
            return true;
        }
        let message = &mut self.last_message;
        match path.as_str() {
            "/message" if replace => {
                let Some(value) = patch_value else {
                    return false;
                };
                *message = MessageDiagnostic::from_message(value, "patch");
            }
            "/message/status" if replace => message.status = string_tag(patch_value),
            "/message/end_turn" if replace => {
                message.end_turn = patch_value.and_then(Value::as_bool)
            }
            "/message/channel" if replace => message.channel = channel_tag(patch_value),
            "/message/recipient" if replace => message.recipient = string_tag(patch_value),
            "/message/author" | "/message/author/role" if replace => {
                let role = if path.ends_with("/role") {
                    patch_value
                } else {
                    patch_value.and_then(|value| value.get("role"))
                };
                if string_tag(role) != message.role {
                    *message = MessageDiagnostic::default();
                }
                message.role = string_tag(role);
            }
            "/message/content" if replace => message.observe_content(patch_value),
            "/message/content/content_type" if replace => {
                message.content_type = string_tag(patch_value)
            }
            "/message/content/parts" if replace => {
                message.text = TextPrefix::default();
                message.part_types.clear();
                message.observe_parts(patch_value);
            }
            path if path.starts_with("/message/content/parts/") => {
                let index = path.trim_start_matches("/message/content/parts/");
                // Text deltas for the first part are the only incremental text
                // we assemble. Other shapes remain visible in image hints.
                if index == "0" {
                    if replace {
                        message.text = TextPrefix::default();
                        message.part_types.clear();
                    }
                    if !message.single_text_part {
                        message.text.complete = None;
                    }
                    if let Some(value) = patch_value {
                        message.observe_part(value);
                    }
                } else {
                    // The diagnostic prefix supports only the first text delta.
                    // Never classify an incompletely reconstructed message.
                    message.single_text_part = false;
                    message.text.complete = None;
                }
            }
            _ => return false,
        }
        message.source = Some("patch");
        true
    }

    pub(super) fn has_current_message(&self) -> bool {
        self.last_message.source.is_some()
    }

    pub(super) fn current_assistant_text(&self) -> Option<&str> {
        let message = &self.last_message;
        (message.anchored && message.role.as_deref() == Some("assistant"))
            .then_some(message.text.complete.as_deref())
            .flatten()
    }

    pub(super) fn completed_assistant_text(&self) -> Option<(&str, bool)> {
        let message = &self.last_message;
        if !message.is_terminal() || self.json_error_count > 0 {
            return None;
        }
        if !matches!(message.content_type.as_deref(), Some("text" | "refusal"))
            || message
                .part_types
                .iter()
                .any(|kind| !matches!(kind.as_str(), "text" | "output_text" | "refusal"))
        {
            return None;
        }
        let text = message.text.complete.as_deref()?.trim();
        (!text.is_empty()).then_some((
            text,
            message.content_type.as_deref() == Some("refusal")
                || message.part_types.iter().any(|kind| kind == "refusal"),
        ))
    }

    pub(super) fn merge_from(&mut self, other: &Self) {
        self.json_error_count = self.json_error_count.saturating_add(other.json_error_count);
        if other.last_message.source.is_some() {
            self.last_message = other.last_message.clone();
            self.terminal_reason = other.terminal_reason;
        }
        if other.last_assistant.is_some() {
            self.last_assistant = other.last_assistant.clone();
        }
        if other.terminal_reason.is_some() {
            self.terminal_reason = other.terminal_reason;
        }
        self.image_hints.merge_from(&other.image_hints);
    }

    pub(super) fn resolved_urls(&mut self, count: usize) {
        self.url_resolution_attempts = self.url_resolution_attempts.saturating_add(1);
        self.resolved_url_observations =
            self.resolved_url_observations.saturating_add(count as u64);
    }

    pub(super) fn download_started(&mut self) {
        self.download_attempts = self.download_attempts.saturating_add(1);
    }

    pub(super) fn download_failed(&mut self) {
        self.download_failures = self.download_failures.saturating_add(1);
    }

    pub(super) fn emit_no_output(&self, plan: &ExecutionPlan, summary: &WebImageSseSummary) {
        let empty = MessageDiagnostic::default();
        let assistant = self.last_assistant.as_ref().unwrap_or(&empty);
        let excerpt = assistant.text.excerpt();
        warn!(
            event_name = "chatgpt_web_image_no_output_diagnostics",
            request_id = %plan.request_id,
            candidate_id = ?plan.candidate_id,
            key_id = %plan.key_id,
            terminal_reason = ?self.terminal_reason,
            turn_finished = summary.turn_finished,
            message_state_anchored = self.last_message.anchored,
            last_assistant_source = ?assistant.source,
            last_assistant_recipient = ?assistant.recipient,
            last_assistant_channel = ?assistant.channel,
            last_assistant_status = ?assistant.status,
            last_assistant_end_turn = ?assistant.end_turn,
            last_assistant_content_type = ?assistant.content_type,
            last_assistant_part_types = %serde_json::to_string(&assistant.part_types).unwrap_or_default(),
            last_assistant_text_excerpt = ?excerpt,
            text_prefix_truncated = assistant.text.truncated,
            image_structure_observations = self.image_hints.image_structure_observations,
            file_reference_observations = self.image_hints.file_reference_observations,
            sediment_reference_observations = self.image_hints.sediment_reference_observations,
            image_url_observations = self.image_hints.image_url_observations,
            data_image_observations = self.image_hints.data_image_observations,
            encoded_result_observations = self.image_hints.encoded_result_observations,
            embedded_reference_observations = self.image_hints.embedded_reference_observations,
            image_hint_scan_truncated = self.image_hints.truncated,
            extracted_file_count = summary.file_ids.len(),
            extracted_sediment_count = summary.sediment_ids.len(),
            extracted_direct_url_count = summary.direct_urls.len(),
            url_resolution_attempts = self.url_resolution_attempts,
            resolved_url_observations = self.resolved_url_observations,
            download_attempts = self.download_attempts,
            download_failures = self.download_failures,
            "ChatGPT-Web image attempt has no output; bounded message and asset evidence"
        );
    }
}

// Only full messages and the active poll node can establish message state.
// An arbitrary nested object named "message" in a delta is not a snapshot.
pub(super) fn current_message(value: &Value) -> Option<&Value> {
    match value.get("p").and_then(Value::as_str) {
        Some("/message")
            if matches!(
                value.get("o").and_then(Value::as_str),
                Some("add" | "replace")
            ) =>
        {
            return value.get("v").filter(|value| value.is_object());
        }
        Some(path) if !path.is_empty() && path != "/" => return None,
        _ => {}
    }
    let value = value
        .get("v")
        .filter(|value| value.is_object())
        .unwrap_or(value);
    if let Some(current) = value.get("current_node").and_then(Value::as_str) {
        return value.get("mapping")?.get(current)?.get("message");
    }
    if value.get("mapping").is_some() {
        return None;
    }
    value.get("message").filter(|message| message.is_object())
}

impl MessageDiagnostic {
    fn from_message(message: &Value, source: &'static str) -> Self {
        let mut result = Self {
            id: message
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| id.len() <= 256)
                .map(str::to_owned),
            anchored: message
                .pointer("/author/role")
                .and_then(Value::as_str)
                .is_some(),
            source: Some(source),
            role: string_tag(message.pointer("/author/role")),
            recipient: string_tag(message.get("recipient")),
            channel: channel_tag(message.get("channel")),
            status: string_tag(message.get("status")),
            end_turn: message.get("end_turn").and_then(Value::as_bool),
            ..Self::default()
        };
        result.observe_content(message.get("content"));
        result
    }

    fn is_terminal(&self) -> bool {
        self.anchored
            && self.role.as_deref() == Some("assistant")
            && self.recipient.as_deref() == Some("all")
            && matches!(self.channel.as_deref(), None | Some("final" | "commentary"))
            && self.status.as_deref() == Some("finished_successfully")
            && self.end_turn == Some(true)
    }

    fn observe_content(&mut self, content: Option<&Value>) {
        self.content_type = string_tag(content.and_then(|content| content.get("content_type")));
        self.part_types.clear();
        self.text = TextPrefix::default();
        self.observe_parts(content.and_then(|content| content.get("parts")));
        if let Some(text) = content
            .and_then(|content| content.get("text").or_else(|| content.get("refusal")))
            .and_then(Value::as_str)
        {
            self.text.append(text);
        }
    }

    fn observe_parts(&mut self, parts: Option<&Value>) {
        self.single_text_part = parts
            .and_then(Value::as_array)
            .is_some_and(|parts| parts.len() == 1);
        if let Some(parts) = parts.and_then(Value::as_array) {
            for part in parts.iter().take(CONTENT_PART_LIMIT) {
                self.observe_part(part);
            }
            self.text.truncated |= parts.len() > CONTENT_PART_LIMIT;
            if parts.len() > CONTENT_PART_LIMIT {
                self.text.complete = None;
            }
        }
    }

    fn observe_part(&mut self, part: &Value) {
        let kind = if part.is_string() {
            "text".to_string()
        } else {
            string_tag(part.get("content_type"))
                .or_else(|| string_tag(part.get("type")))
                .unwrap_or_else(|| "other".to_string())
        };
        if self.part_types.len() < CONTENT_PART_LIMIT && !self.part_types.contains(&kind) {
            self.part_types.push(kind);
        }
        let text = part.as_str().or_else(|| {
            matches!(
                part.get("content_type")
                    .or_else(|| part.get("type"))
                    .and_then(Value::as_str),
                Some("text" | "output_text" | "refusal")
            )
            .then(|| {
                part.get("text")
                    .or_else(|| part.get("refusal"))
                    .and_then(Value::as_str)
            })
            .flatten()
        });
        if let Some(text) = text {
            self.text.append(text);
        }
    }
}

impl ImageHints {
    fn observe(&mut self, value: &Value, key: Option<&str>, depth: usize) {
        if self.nodes >= IMAGE_SCAN_NODE_LIMIT || depth >= IMAGE_SCAN_DEPTH_LIMIT {
            self.truncated = true;
            return;
        }
        self.nodes += 1;
        match value {
            Value::Object(object) => {
                // Input uploads echoed in a user message are not output evidence.
                if matches!(
                    value.pointer("/author/role").and_then(Value::as_str),
                    Some("user" | "system" | "developer")
                ) {
                    return;
                }
                let kind = value
                    .get("content_type")
                    .or_else(|| value.get("type"))
                    .and_then(Value::as_str);
                if matches!(
                    kind,
                    Some(
                        "image_asset_pointer"
                            | "image"
                            | "image_url"
                            | "output_image"
                            | "image_generation_call"
                    )
                ) || object.contains_key("asset_pointer")
                    || object.contains_key("image_url")
                {
                    self.image_structure_observations =
                        self.image_structure_observations.saturating_add(1);
                }
                for (key, child) in object {
                    if matches!(key.as_str(), "b64_json" | "result")
                        && child.as_str().is_some_and(|value| !value.is_empty())
                        && (key == "b64_json" || kind == Some("image_generation_call"))
                    {
                        self.encoded_result_observations =
                            self.encoded_result_observations.saturating_add(1);
                    }
                    let context = if key == "url" && kind == Some("image_url") {
                        "image_url"
                    } else {
                        key
                    };
                    self.observe(child, Some(context), depth + 1);
                    if self.nodes >= IMAGE_SCAN_NODE_LIMIT {
                        self.truncated = true;
                        break;
                    }
                }
            }
            Value::Array(values) => {
                for child in values {
                    self.observe(child, key, depth + 1);
                    if self.nodes >= IMAGE_SCAN_NODE_LIMIT {
                        self.truncated = true;
                        break;
                    }
                }
            }
            Value::String(text) => {
                if text.starts_with("sediment://") {
                    self.sediment_reference_observations =
                        self.sediment_reference_observations.saturating_add(1);
                } else if text.starts_with("file-") {
                    self.file_reference_observations =
                        self.file_reference_observations.saturating_add(1);
                } else if text.starts_with("data:image/") {
                    self.data_image_observations = self.data_image_observations.saturating_add(1);
                } else if text.len() <= 4096
                    && text.starts_with("http")
                    && (matches!(key, Some("image_url" | "download_url" | "asset_pointer"))
                        || is_generated_web_asset_url(text))
                {
                    self.image_url_observations = self.image_url_observations.saturating_add(1);
                } else {
                    let prefix = &text.as_bytes()[..text.len().min(512)];
                    if [b"sediment://".as_slice(), b"file-", b"data:image/"]
                        .iter()
                        .any(|needle| prefix.windows(needle.len()).any(|part| part == *needle))
                    {
                        self.embedded_reference_observations =
                            self.embedded_reference_observations.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
    }

    fn merge_from(&mut self, other: &Self) {
        self.truncated |= other.truncated;
        self.image_structure_observations = self
            .image_structure_observations
            .saturating_add(other.image_structure_observations);
        self.file_reference_observations = self
            .file_reference_observations
            .saturating_add(other.file_reference_observations);
        self.sediment_reference_observations = self
            .sediment_reference_observations
            .saturating_add(other.sediment_reference_observations);
        self.image_url_observations = self
            .image_url_observations
            .saturating_add(other.image_url_observations);
        self.data_image_observations = self
            .data_image_observations
            .saturating_add(other.data_image_observations);
        self.encoded_result_observations = self
            .encoded_result_observations
            .saturating_add(other.encoded_result_observations);
        self.embedded_reference_observations = self
            .embedded_reference_observations
            .saturating_add(other.embedded_reference_observations);
    }
}

pub(super) struct InitialResponseDiagnostics<'a> {
    plan: &'a ExecutionPlan,
    started_at: Instant,
    finished: bool,
    pub(super) read: BufferedBodyReadDiagnostics,
}

impl<'a> InitialResponseDiagnostics<'a> {
    pub(super) fn new(plan: &'a ExecutionPlan) -> Self {
        Self {
            plan,
            started_at: Instant::now(),
            finished: false,
            read: BufferedBodyReadDiagnostics::default(),
        }
    }

    pub(super) fn finish(
        &mut self,
        summary: Option<&WebImageSseSummary>,
        error: Option<&ExecutionRuntimeTransportError>,
    ) {
        self.finished = true;
        self.emit(
            if error.is_some() {
                "failed"
            } else {
                "completed"
            },
            summary,
            error,
        );
    }

    fn emit(
        &self,
        outcome: &'static str,
        summary: Option<&WebImageSseSummary>,
        error: Option<&ExecutionRuntimeTransportError>,
    ) {
        let empty = WebImageSseDiagnostics::default();
        let sse = summary
            .map(|summary| &summary.diagnostics)
            .unwrap_or(&empty);
        let elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        let read_end = if self.read.reached_eof {
            "eof"
        } else if self.read.status_code.is_some() {
            "interrupted"
        } else {
            "no_response_headers"
        };
        let error_kind = error.map(|error| match error {
            ExecutionRuntimeTransportError::UpstreamHttpStatus { .. } => "http_status",
            ExecutionRuntimeTransportError::UpstreamRequest(_) => "upstream_request",
            ExecutionRuntimeTransportError::BrowserBody(_) => "body_read",
            ExecutionRuntimeTransportError::UpstreamResponseTooLarge { .. } => "body_limit",
            ExecutionRuntimeTransportError::UpstreamResponseDecode { .. } => "response_decode",
            _ => "other",
        });
        let upstream_error = error.and_then(|error| match error {
            ExecutionRuntimeTransportError::UpstreamHttpStatus {
                response_body: Some(body),
                ..
            } => body.get("error"),
            _ => None,
        });
        info!(
            event_name = "chatgpt_web_image_initial_response_diagnostics",
            request_id = %self.plan.request_id,
            candidate_id = ?self.plan.candidate_id,
            key_id = %self.plan.key_id,
            outcome,
            read_end,
            elapsed_ms,
            status_code = ?self.read.status_code,
            headers_ms = ?self.read.headers_ms,
            first_chunk_ms = ?self.read.first_chunk_ms,
            last_chunk_ms = ?self.read.last_chunk_ms,
            max_chunk_gap_ms = self.read.max_chunk_gap_ms,
            idle_after_last_chunk_ms = ?self.read.last_chunk_ms.map(|last| elapsed_ms.saturating_sub(last)),
            chunk_count = self.read.chunk_count,
            received_bytes = self.read.received_bytes,
            sse_parsed = summary.is_some(),
            response_bytes = sse.response_bytes,
            event_count = sse.event_count,
            json_event_count = sse.json_event_count,
            json_error_count = sse.json_error_count,
            done_marker_count = sse.done_marker_count,
            comment_line_count = sse.comment_line_count,
            patch_event_count = sse.patch_event_count,
            utf8_lossy = sse.utf8_lossy,
            trailing_event_without_separator = sse.trailing_event_without_separator,
            event_tail = %serde_json::to_string(&sse.event_tail).unwrap_or_default(),
            last_message_source = ?sse.last_message.source,
            last_message_role = ?sse.last_message.role,
            last_message_recipient = ?sse.last_message.recipient,
            last_message_channel = ?sse.last_message.channel,
            last_message_status = ?sse.last_message.status,
            last_message_end_turn = ?sse.last_message.end_turn,
            last_json_error_category = ?sse.last_json_error.as_ref().map(|error| error.category),
            last_json_error_line = ?sse.last_json_error.as_ref().map(|error| error.line),
            last_json_error_column = ?sse.last_json_error.as_ref().map(|error| error.column),
            turn_finished = ?summary.map(|summary| summary.turn_finished),
            asset_count = ?summary.map(WebImageSseSummary::retained_item_count),
            error_kind,
            upstream_error_type = ?upstream_error.and_then(|body| body.get("type")).and_then(|value| value.as_str()),
            upstream_error_code = ?upstream_error.and_then(|body| body.get("code")).and_then(|value| value.as_str()),
            upstream_error_param = ?upstream_error.and_then(|body| body.get("param")).and_then(|value| value.as_str()),
            upstream_error_message = ?upstream_error.and_then(|body| body.get("message")).and_then(|value| value.as_str()),
            error = ?error.map(safe_transport_error_message),
            "ChatGPT-Web initial response read and SSE diagnostics"
        );
    }
}

impl Drop for InitialResponseDiagnostics<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // Total timeout cancels the read future before it can return an error.
            self.emit("cancelled", None, None);
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn audit_same_message_id_patch_keeps_terminal_anchor_and_full_text() {
        let mut message = assistant_message();
        message["id"] = json!("assistant-one");
        message["channel"] = json!("commentary");
        message["content"]["parts"] = json!(["U"]);
        let summary = summary_events([
            json!({"message":message}),
            json!({"p":"/message/id","o":"replace","v":"assistant-one"}),
            json!({"p":"/message/content/parts/0","o":"append","v":"nable to generate an image."}),
        ]);
        assert!(summary.turn_finished);
        assert_eq!(
            summary.last_text.as_deref(),
            Some("Unable to generate an image.")
        );
    }

    #[test]
    fn audit_new_message_id_and_transport_done_cannot_reuse_old_terminal() {
        let mut message = assistant_message();
        message["id"] = json!("assistant-one");
        let summary = summary_events([
            json!({"message":message}),
            json!({"p":"/message/id","o":"replace","v":"assistant-two"}),
        ]);
        assert!(!summary.turn_finished);
        assert!(summary.diagnostics.completed_assistant_text().is_none());
        assert!(!parse_web_image_sse(b"data: [DONE]\n\n").turn_finished);
    }

    use super::*;

    #[test]
    fn wrapped_polls_with_an_unavailable_active_node_clear_stale_terminal_state() {
        let mut target = summary_events([json!({"message":assistant_message()})]);
        let mut source = WebImageSseSummary::default();
        observe_web_image_progress(
            &json!({"v":{"current_node":"missing","mapping":{}}}),
            &mut source,
        );
        merge_web_summary(&mut target, &mut source);
        assert!(!target.turn_finished);
    }

    #[test]
    fn structured_text_and_refusal_parts_produce_a_readable_excerpt() {
        let mut message = assistant_message();
        message["content"]["parts"] = json!([
            {"content_type":"text","text":"Unable to "},
            {"type":"refusal","refusal":"generate an image."}
        ]);
        let summary = summary_events([json!({"message":message})]);
        let assistant = summary.diagnostics.last_assistant.unwrap();
        assert_eq!(
            assistant.text.excerpt().as_deref(),
            Some("Unable to generate an image.")
        );
        assert_eq!(assistant.part_types, ["text", "refusal"]);
    }

    #[test]
    fn terminal_patches_have_a_global_work_budget_and_known_assets_still_poll() {
        let group = json!({"o":"patch","v":vec![json!({"p":"/metadata/unused","o":"replace","v":true}); 8]});
        let patch = json!({"o":"patch","v":vec![group; 8]});
        let summary = summary_events([json!({"message":assistant_message()}), patch]);
        assert!(!summary.turn_finished);
        assert!(!summary.diagnostics.last_message.anchored);
        let summary = summary_events([
            json!({"conversation_id":"conv-test-1", "message":assistant_message(), "asset":"file-generated-123456"}),
        ]);
        assert!(summary.turn_finished);
        assert!(should_poll_web_image(&summary));
    }

    fn summary_events(events: impl IntoIterator<Item = Value>) -> WebImageSseSummary {
        let mut body = String::new();
        for event in events {
            body.push_str(&format!("data: {event}\n\n"));
        }
        body.push_str("data: [DONE]\n\n");
        parse_web_image_sse(body.as_bytes())
    }

    fn assistant_message() -> Value {
        json!({"author":{"role":"assistant"},"recipient":"all", "status":"finished_successfully", "end_turn":true,
            "content":{"content_type":"text","parts":["Unable to generate an image."]}})
    }

    #[test]
    fn terminal_without_channel_requires_all_other_terminal_metadata() {
        for channel in [
            None,
            Some(Value::Null),
            Some(json!("final")),
            Some(json!("commentary")),
        ] {
            let mut message = assistant_message();
            if let Some(channel) = channel {
                message["channel"] = channel;
            }
            let summary =
                summary_events([json!({"v":{"message":message}, "conversation_id":"conv-test-1"})]);
            assert!(summary.turn_finished);
            assert!(!should_poll_web_image(&summary));
        }
        for (field, value) in [
            ("channel", json!("analysis")),
            ("channel", json!(123)),
            ("recipient", json!("imagegen")),
            ("status", json!("in_progress")),
            ("end_turn", json!(false)),
            ("author", json!({"role":"tool"})),
        ] {
            let mut message = assistant_message();
            message[field] = value;
            let summary =
                summary_events([json!({"message":message, "conversation_id":"conv-test-1"})]);
            assert!(!summary.turn_finished, "field {field}");
            assert!(should_poll_web_image(&summary), "field {field}");
        }
    }

    #[test]
    fn relative_terminal_patches_require_an_anchor_and_reset_with_a_new_message() {
        let patch = json!({"p":"/message","o":"patch","v":[
            {"p":"/status","o":"replace","v":"finished_successfully"},
            {"p":"/end_turn","o":"replace","v":true}
        ]});
        assert!(!summary_events([patch.clone()]).turn_finished);
        let mut message = assistant_message();
        message["status"] = json!("in_progress");
        message["end_turn"] = json!(false);
        let snapshot = json!({"p":"","o":"add","v":{"message":message}});
        assert!(summary_events([snapshot.clone(), patch.clone()]).turn_finished);
        let id_change = json!({"p":"/message/id","o":"replace","v":"new-message"});
        assert!(
            !summary_events([snapshot.clone(), patch.clone(), id_change, patch.clone()])
                .turn_finished
        );
        let tool = json!({"p":"/message","o":"replace","v":{"author":{"role":"tool"},"status":"in_progress"}});
        assert!(!summary_events([snapshot, patch.clone(), tool, patch]).turn_finished);
    }

    #[test]
    fn too_many_or_deep_patches_cannot_confirm_a_terminal() {
        let mut patches = vec![json!({"p":"/message/end_turn","o":"replace","v":true}); 17];
        patches[16] = json!({"p":"/message/status","o":"replace","v":"in_progress"});
        let summary = summary_events([
            json!({"message":assistant_message()}),
            json!({"o":"patch","v":patches}),
        ]);
        assert!(!summary.turn_finished);
        let mut patch = json!({"p":"/message/status","o":"replace","v":"in_progress"});
        for _ in 0..5 {
            patch = json!({"o":"patch","v":[patch]});
        }
        assert!(!summary_events([json!({"message":assistant_message()}), patch]).turn_finished);
    }

    #[test]
    fn last_assistant_content_survives_a_later_tool_and_text_deltas() {
        let mut message = assistant_message();
        message["content"]["parts"] = json!(["Unable to "]);
        let summary = summary_events([
            json!({"v":{"message":message}}),
            json!({"p":"/message/content/parts/0","o":"append","v":"generate an image."}),
            json!({"message":{"author":{"role":"tool"},"content":{"content_type":"multimodal_text","parts":[{"content_type":"image_asset_pointer", "asset_pointer":"unknown-resource://test"}]}}}),
        ]);
        let d = summary.diagnostics;
        assert_eq!(d.last_message.role.as_deref(), Some("tool"));
        let assistant = d.last_assistant.unwrap();
        assert_eq!(assistant.content_type.as_deref(), Some("text"));
        assert_eq!(assistant.part_types, ["text"]);
        assert_eq!(
            assistant.text.excerpt().as_deref(),
            Some("Unable to generate an image.")
        );
        assert_eq!(d.image_hints.image_structure_observations, 1);
    }

    #[test]
    fn diagnostic_excerpt_is_bounded_unicode_safe_and_redacts_private_values() {
        let mut text = TextPrefix::default();
        text.append("无法生成图片\nhttps://assets.example.test/private.png?token=secret alice@example.test Bearer token-value password=top-secret ");
        text.append(&"A".repeat(120));
        text.append(&"。".repeat(2000));
        assert!(text.truncated);
        assert!(text.value.len() <= TEXT_PREFIX_BYTES);
        let excerpt = text.excerpt().unwrap();
        assert!(excerpt.starts_with("无法生成图片"));
        assert!(excerpt.len() <= 259);
        assert!(!excerpt.contains('\n'));
        for secret in [
            "https://",
            "private.png",
            "alice@",
            "token-value",
            "top-secret",
            &"A".repeat(40),
        ] {
            assert!(!excerpt.contains(secret), "leaked {secret}: {excerpt}");
        }
        let debug = format!("{text:?}");
        assert!(!debug.contains("top-secret"));
        assert!(!debug.contains("无法生成图片"));
    }

    #[test]
    fn image_hints_detect_unextracted_structures_and_embedded_references() {
        let summary = summary_events([
            json!({"message":{"author":{"role":"assistant"}, "content":{
                "content_type":"multimodal_text", "parts":[
                    {"content_type":"image_asset_pointer", "asset_pointer":"new-resource-format://opaque"},
                    "![generated](sediment://embedded-resource)"
                ]
            }}}),
        ]);
        assert_eq!(summary.retained_item_count(), 0);
        assert_eq!(
            summary.diagnostics.image_hints.image_structure_observations,
            1
        );
        assert_eq!(
            summary
                .diagnostics
                .image_hints
                .embedded_reference_observations,
            1
        );
        assert!(!summary.diagnostics.image_hints.truncated);
    }

    #[test]
    fn image_hint_counts_are_observations_and_ignore_echoed_user_uploads() {
        let output = json!({"message":{"author":{"role":"tool"},"content":{"parts":[
            {"content_type":"image_asset_pointer","asset_pointer":"file-generated-123456"}
        ]}}});
        let input = json!({"message":{"author":{"role":"user"},"content":{"parts":[
            {"content_type":"image_asset_pointer","asset_pointer":"file-input-123456"}
        ]}}});
        let summary = summary_events([input, output.clone(), output]);
        assert_eq!(
            summary.diagnostics.image_hints.file_reference_observations,
            2
        );
        assert_eq!(
            summary.diagnostics.image_hints.image_structure_observations,
            2
        );
    }

    #[test]
    fn image_hint_scan_and_content_capture_are_bounded() {
        let value = json!({"parts":vec![json!({"content_type":"image_asset_pointer"}); IMAGE_SCAN_NODE_LIMIT * 2]});
        let mut hints = ImageHints::default();
        hints.observe(&value, None, 0);
        assert_eq!(hints.nodes, IMAGE_SCAN_NODE_LIMIT);
        assert!(hints.truncated);
        let mut message = assistant_message();
        message["content"]["parts"] = json!(vec!["x"; CONTENT_PART_LIMIT + 10]);
        let d = MessageDiagnostic::from_message(&message, "snapshot");
        assert_eq!(d.text.value.len(), CONTENT_PART_LIMIT);
        assert!(d.text.truncated);
    }

    #[test]
    fn poll_diagnostics_ignore_history_and_replace_the_previous_assistant_excerpt() {
        let mut target = summary_events([json!({"message":assistant_message()})]);
        let mut current = assistant_message();
        current["content"]["parts"] = json!(["Please try again later."]);
        let poll = json!({"current_node":"current", "mapping":{
            "old":{"message":{"author":{"role":"tool"},"content":{"parts":[{"content_type":"image_asset_pointer","asset_pointer":"file-old-123456"}]}}},
            "current":{"message":current}
        }});
        let mut source = WebImageSseSummary::default();
        source.diagnostics.observe_json(None, &poll);
        observe_web_image_progress(&poll, &mut source);
        merge_web_summary(&mut target, &mut source);
        assert!(target.turn_finished);
        assert_eq!(target.last_text.as_deref(), Some("Please try again later."));
        assert_eq!(
            target.diagnostics.image_hints.file_reference_observations,
            0
        );
        assert_eq!(
            target
                .diagnostics
                .last_assistant
                .unwrap()
                .text
                .excerpt()
                .as_deref(),
            Some("Please try again later.")
        );
    }

    #[test]
    fn sse_diagnostics_tracks_multiline_crlf_events_and_done() {
        let body = concat!(
            ": keepalive\r\n\r\n",
            "event: message\r\ndata: {\r\ndata: \"conversation_id\":\"conv-test-1\",\r\n",
            "data: \"message\":{\"author\":{\"role\":\"assistant\"},\"recipient\":\"all\",",
            "\"channel\":\"final\",\"status\":\"finished_successfully\",\"end_turn\":true}}\r\n\r\n",
            "data: [DONE]\r\n\r\n"
        );
        let summary = parse_web_image_sse(body.as_bytes());
        assert_eq!(summary.conversation_id.as_deref(), Some("conv-test-1"));
        assert!(summary.turn_finished);
        let d = summary.diagnostics;
        assert_eq!(d.response_bytes, body.len());
        assert_eq!(d.event_count, 2);
        assert_eq!(d.json_event_count, 1);
        assert_eq!(d.done_marker_count, 1);
        assert_eq!(d.comment_line_count, 1);
        assert_eq!(d.event_tail[0].event.as_deref(), Some("message"));
        assert_eq!(
            d.last_message.status.as_deref(),
            Some("finished_successfully")
        );
        assert_eq!(d.last_message.end_turn, Some(true));
        assert!(!d.utf8_lossy);
        assert!(!d.trailing_event_without_separator);
    }

    #[test]
    fn sse_diagnostics_distinguishes_eof_without_done_and_unterminated_event() {
        for (ending, trailing) in [("\n\n", false), ("", true)] {
            let body = format!("data: {{\"conversation_id\":\"conv-test-1\"}}{ending}");
            let summary = parse_web_image_sse(body.as_bytes());
            assert_eq!(summary.conversation_id.as_deref(), Some("conv-test-1"));
            assert!(!summary.turn_finished);
            assert_eq!(summary.diagnostics.event_count, 1);
            assert_eq!(summary.diagnostics.done_marker_count, 0);
            assert_eq!(
                summary.diagnostics.trailing_event_without_separator,
                trailing
            );
        }
    }

    #[test]
    fn sse_diagnostics_counts_invalid_json_without_losing_assets_or_raw_data() {
        let mut body =
            b"data: {broken-json}\n\ndata: {\"asset\":\"file-generated-123456\"}\n\n: ".to_vec();
        body.push(0xff);
        body.extend_from_slice(b"\n\ndata: {\"incomplete\":");
        let summary = parse_web_image_sse(&body);
        assert_eq!(summary.file_ids, ["file-generated-123456"]);
        assert_eq!(summary.diagnostics.response_bytes, body.len());
        assert_eq!(summary.diagnostics.event_count, 3);
        assert_eq!(summary.diagnostics.json_error_count, 2);
        assert_eq!(summary.diagnostics.json_event_count, 1);
        assert!(summary.diagnostics.utf8_lossy);
        assert!(summary.diagnostics.trailing_event_without_separator);
        let error = summary.diagnostics.last_json_error.unwrap();
        assert_eq!(error.category, "eof");
        assert!(error.line > 0);
        assert!(error.column > 0);
    }

    #[test]
    fn sse_diagnostics_tail_is_bounded_and_does_not_retain_payloads() {
        let mut body = String::new();
        for index in 0..20 {
            body.push_str(&format!(
                "event: delta\ndata: {{\"type\":\"event_{index}\",\"content\":\"private-prompt\",\"token\":\"test-access-token\",\"image\":\"iVBORw0KGgo\"}}\n\n"
            ));
        }
        let summary = parse_web_image_sse(body.as_bytes());
        let d = summary.diagnostics;
        assert_eq!(d.event_count, 20);
        assert_eq!(d.event_tail.len(), EVENT_TAIL_LIMIT);
        assert_eq!(d.event_tail[0].event_type.as_deref(), Some("event_12"));
        let retained = format!("{d:?}");
        for secret in ["private-prompt", "test-access-token", "iVBORw0KGgo"] {
            assert!(!retained.contains(secret), "retained {secret}: {retained}");
        }
        for unsafe_tag in [
            "line\nbreak",
            "https://example.test/image",
            "中文",
            &"x".repeat(81),
        ] {
            assert_eq!(diagnostic_event_tag(unsafe_tag), "<nonstandard>");
        }
        assert_eq!(
            diagnostic_event_tag("response.output_item.done"),
            "response.output_item.done"
        );
    }

    #[test]
    fn sse_diagnostics_observes_v1_snapshots_and_applies_terminal_metadata() {
        let body = concat!(
            "data: {\"p\":\"\",\"o\":\"add\",\"v\":{\"message\":{\"author\":{\"role\":\"assistant\"},",
            "\"recipient\":\"all\",\"channel\":\"final\",\"status\":\"in_progress\",\"end_turn\":false}}}\n\n",
            "data: {\"o\":\"patch\",\"v\":[{\"p\":\"/message/status\",\"o\":\"replace\",\"v\":\"finished_successfully\"},",
            "{\"p\":\"/message/end_turn\",\"o\":\"replace\",\"v\":true}]}\n\n",
            "data: [DONE]\n\n"
        );
        let summary = parse_web_image_sse(body.as_bytes());
        assert!(
            summary.turn_finished,
            "anchored terminal metadata patches must update turn state"
        );
        assert_eq!(summary.retained_item_count(), 0);
        let d = summary.diagnostics;
        assert_eq!(d.patch_event_count, 2);
        assert_eq!(d.event_tail[0].shape, "v1_message");
        assert_eq!(d.event_tail[1].shape, "patch");
        assert_eq!(d.last_message.source, Some("patch"));
        assert_eq!(d.last_message.role.as_deref(), Some("assistant"));
        assert_eq!(d.last_message.channel.as_deref(), Some("final"));
        assert_eq!(
            d.last_message.status.as_deref(),
            Some("finished_successfully")
        );
        assert_eq!(d.last_message.end_turn, Some(true));
    }
}
