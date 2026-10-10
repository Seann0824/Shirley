//! 会话结束后的增量 curator（`docs/memory.md` §5）。
//!
//! 写入路径不靠"AI 主动调工具"（那看模型心情），而是**会话结束时**由 curator 读
//! 轨迹、产出尽可能小的 diff（新条目 / 取代关系），再过一道**确定性自检**合入。
//!
//! V1 的三条取舍（§5.3）：
//! - **同模型**：用 [`Agent::complete`]（一次性、无工具、非流式、不碰会话日志）。
//!   异源审核（独立模型）留到 V2。
//! - **确定性自检**：不靠 LLM，只查 schema / `source` / `supersedes` / 时间 / 链接。
//!   **不通过就不合入，绝不默认放行**。
//! - **诚实降级**：只读探测（§5.4）V1 不做，退化为纯轨迹 curation。
//!
//! 边界声明（照抄论文的 distiller 边界）：轨迹是**部分证据**、不是 ground truth——
//! 一次通过不自动验证每个中间假设。这条写进给模型的提示词里。

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use shirley_agent_sdk::{Agent, Message};

use super::format::{Confidence, Entry, EntryStatus, EntryType, MemoryError};
use super::store::MemoryStore;

/// curator 的错误。
#[derive(Debug, thiserror::Error)]
pub enum CuratorError {
    /// 调模型失败（收敛成字符串，避免 curator 依赖 SDK 的错误类型）。
    #[error("curator model call failed: {0}")]
    Model(String),
    /// 模型输出不是合法 JSON。
    #[error("curator output not parseable: {0}")]
    Parse(String),
    /// 落盘失败。
    #[error(transparent)]
    Memory(#[from] MemoryError),
}

/// 一次 curation 的结果。
#[derive(Debug, Default)]
pub struct CurateOutcome {
    /// 成功写入的条目 id。
    pub written: Vec<String>,
    /// 被确定性自检拒绝的候选（id 可能缺失）。
    pub rejected: Vec<Rejected>,
    /// 模型没产出任何候选（正常：这轮没有可沉淀的东西）。
    pub empty: bool,
}

/// 一条被拒绝的候选及原因。
///
/// 供上层观测（`spawn_curation` 会打印计数）；V1 尚无消费方，字段为诊断保留。
#[derive(Debug)]
#[allow(dead_code)]
pub struct Rejected {
    pub id: Option<String>,
    pub reason: String,
}

/// 模型输出的候选条目（与 [`Entry`] 解耦：模型只提供这些字段，其余由 curator 补）。
#[derive(Debug, Clone, Deserialize)]
pub struct Candidate {
    pub id: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub subject: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub valid_from: Option<String>,
    #[serde(default)]
    pub supersedes: Option<String>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub source: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// 从模型文本里抽出候选数组（容忍 ```json 代码围栏与前后废话）。
pub fn parse_candidates(text: &str) -> Result<Vec<Candidate>, CuratorError> {
    let start = text
        .find('[')
        .ok_or_else(|| CuratorError::Parse("no JSON array found".into()))?;
    let end = text
        .rfind(']')
        .ok_or_else(|| CuratorError::Parse("no closing `]`".into()))?;
    if end < start {
        return Err(CuratorError::Parse("malformed JSON array".into()));
    }
    serde_json::from_str(&text[start..=end])
        .map_err(|err| CuratorError::Parse(err.to_string()))
}

/// 确定性自检 + 补全默认字段。通过返回 [`Entry`]，否则返回拒绝原因。
///
/// 自检项（§5.3，不靠 LLM）：
/// - `id` / `subject` / `body` 非空，`id` 是 slug；
/// - 时间字段是 `YYYY-MM-DD`；
/// - `source` 非空（**stamp**：轨迹本就来自该会话，缺省时补上已知的 `session_ref`，
///   这是补全真实来源、不是编造证据）；给了 `sessions_dir` 时还校验源文件存在；
/// - `supersedes` 指向的 id 确实存在。
pub fn vet(
    candidate: Candidate,
    existing: &HashMap<String, Entry>,
    today: &str,
    session_ref: Option<&str>,
    sessions_dir: Option<&Path>,
) -> Result<Entry, String> {
    let id = candidate.id.trim();
    if id.is_empty() {
        return Err("empty id".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("id `{id}` is not a slug"));
    }
    if candidate.subject.trim().is_empty() {
        return Err("empty subject".into());
    }
    if candidate.body.trim().is_empty() {
        return Err("empty body".into());
    }

    let created_at = candidate
        .created_at
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(today)
        .to_string();
    if !is_date(&created_at) {
        return Err(format!("created_at `{created_at}` is not YYYY-MM-DD"));
    }
    if let Some(valid_from) = candidate.valid_from.as_deref()
        && !is_date(valid_from.trim())
    {
        return Err(format!("valid_from `{valid_from}` is not YYYY-MM-DD"));
    }

    let mut source: Vec<String> = candidate
        .source
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if source.is_empty()
        && let Some(session_ref) = session_ref
    {
        source.push(session_ref.to_string());
    }
    if source.is_empty() {
        return Err("missing source".into());
    }
    if let Some(dir) = sessions_dir {
        for ref_ in &source {
            let file = ref_.split('#').next().unwrap_or(ref_);
            if !dir.join(file).exists() {
                return Err(format!("source `{ref_}` does not resolve to an existing file"));
            }
        }
    }

    if let Some(supersedes) = candidate.supersedes.as_deref() {
        let supersedes = supersedes.trim();
        if supersedes.is_empty() {
            return Err("empty supersedes".into());
        }
        if supersedes == id {
            return Err("entry supersedes itself".into());
        }
        if !existing.contains_key(supersedes) {
            return Err(format!("supersedes `{supersedes}` does not exist"));
        }
    }

    Ok(Entry {
        id: id.to_string(),
        entry_type: candidate.entry_type,
        subject: candidate.subject.trim().to_string(),
        created_at,
        valid_from: candidate
            .valid_from
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
        supersedes: candidate
            .supersedes
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        status: EntryStatus::Active,
        confidence: candidate.confidence.unwrap_or(Confidence::Medium),
        scope: candidate
            .scope
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        utility: None,
        usage_count: None,
        source,
        body: candidate.body.trim().to_string(),
    })
}

fn is_date(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// 把一条消息渲染成 curator 可读的一行（跳过 system / 压缩摘要——不是对话内容）。
pub fn render_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        match message {
            Message::User { content } => {
                out.push_str("## User\n");
                out.push_str(content.trim());
                out.push_str("\n\n");
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if let Some(content) = content.as_deref().filter(|c| !c.trim().is_empty()) {
                    out.push_str("## Assistant\n");
                    out.push_str(content.trim());
                    out.push_str("\n\n");
                }
                for call in tool_calls {
                    out.push_str(&format!("## Tool call: {}\n{}\n\n", call.name, call.arguments));
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                out.push_str(&format!("## Tool result ({tool_call_id})\n"));
                if let Some(content) = content.as_deref() {
                    out.push_str(content.trim());
                }
                out.push('\n');
                out.push('\n');
            }
            Message::System { .. } | Message::ContextSummary { .. } => {}
        }
    }
    out
}

/// 组装 curator 提示词：边界声明 + 现有相关记忆（摘要）+ 轨迹 + 输出格式。
pub fn build_prompt(transcript: &str, existing: &[(String, Entry)], session_ref: &str) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "你是记忆整理器（curator）。读下面这段对话轨迹，产出**尽可能小**的记忆条目增量。\n\n\
         **边界**：轨迹只是对环境的一次局部观察，是部分证据、不是 ground truth——\
         一次成功不自动验证每个中间假设。只沉淀**跨会话仍然成立**的用户事实 / 偏好 / 事件 / 程序；\
         不确定就不要写（或把 confidence 设为 low）。不要推演轨迹里没有的东西。\n\n\
         只输出一个 JSON 数组（可为空 `[]`），每个元素字段：\n\
         - `id`: 稳定 slug（小写字母数字与 `-`），如 `pref-rust-error-style`\n\
         - `type`: `preference` | `fact` | `event` | `procedure`\n\
         - `subject`: 主题（供检索），如 `rust-error-handling`\n\
         - `created_at`: `YYYY-MM-DD`（可省，默认今天）\n\
         - `valid_from`: `YYYY-MM-DD`（可省）\n\
         - `supersedes`: 若取代已有条目，填它的 id（否则省略）\n\
         - `confidence`: `high` | `medium` | `low`\n\
         - `scope`: 适用场景限定（可省）\n\
         - `source`: 证据引用数组，**必填**，格式 `<会话文件>#turn:N`，本次会话填 ",
    );
    prompt.push_str(&format!("`{session_ref}#turn:N`"));
    prompt.push_str("\n- `body`: 正文（Markdown，一句话讲清）\n\n");
    prompt.push_str("不要删除历史：要改旧偏好就新建条目并 `supersedes` 旧 id。\n\n");

    if !existing.is_empty() {
        prompt.push_str("## 现有相关记忆（避免重复；如需取代用其 id）\n");
        for (_, entry) in existing {
            prompt.push_str(&format!(
                "- [{}] {} — {}\n",
                entry.id,
                entry.subject,
                entry.summary()
            ));
        }
        prompt.push('\n');
    }

    prompt.push_str("## 对话轨迹\n");
    prompt.push_str(transcript);
    prompt
}

/// 会话结束触发一次增量 curation。
///
/// - `agent`：用来调模型（同模型，一次性补全）；
/// - `store`：记忆存储（读现有条目、写新条目、重建索引）；
/// - `messages`：本会话全量消息（curator 的输入轨迹）；
/// - `session_ref`：本会话在 `sessions/` 下的相对文件名（用作 `source` 前缀）。
pub async fn curate(
    agent: &Agent,
    store: &MemoryStore,
    messages: &[Message],
    session_ref: &str,
    sessions_dir: Option<&Path>,
) -> Result<CurateOutcome, CuratorError> {
    let existing_pairs: Vec<(String, Entry)> = store
        .list_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    let existing_by_id: HashMap<String, Entry> = existing_pairs
        .iter()
        .map(|(_, entry)| (entry.id.clone(), entry.clone()))
        .collect();

    let transcript = render_transcript(messages);
    if transcript.trim().is_empty() {
        return Ok(CurateOutcome {
            empty: true,
            ..Default::default()
        });
    }
    let prompt = build_prompt(&transcript, &existing_pairs, session_ref);

    let raw = agent
        .complete(&prompt)
        .await
        .map_err(|err| CuratorError::Model(err.to_string()))?;
    let candidates = parse_candidates(&raw)?;

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut outcome = CurateOutcome {
        empty: candidates.is_empty(),
        ..Default::default()
    };

    for candidate in candidates {
        let candidate_id = Some(candidate.id.clone());
        match vet(
            candidate,
            &existing_by_id,
            &today,
            Some(session_ref),
            sessions_dir,
        ) {
            Ok(entry) => {
                // 取代：旧条目保留，只改状态（`docs/memory.md` 决策 4：永不删历史）。
                if let Some(old_id) = entry.supersedes.clone()
                    && let Some(old) = existing_by_id.get(&old_id)
                {
                    let mut old = old.clone();
                    if old.status != EntryStatus::Superseded {
                        old.status = EntryStatus::Superseded;
                        store.write_entry(&old)?;
                    }
                }
                store.write_entry(&entry)?;
                outcome.written.push(entry.id.clone());
            }
            Err(reason) => outcome.rejected.push(Rejected {
                id: candidate_id,
                reason,
            }),
        }
    }

    rebuild_index(store)?;
    Ok(outcome)
}

/// 用当前全部条目重建 `index.md`（curator 合入后调用）。
pub fn rebuild_index(store: &MemoryStore) -> Result<(), MemoryError> {
    let entries: Vec<(String, Entry)> = store
        .list_entries()?
        .into_iter()
        .map(|(path, entry)| (store.relative_path(&path), entry))
        .collect();
    super::index::write_index(store, &entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str) -> Candidate {
        Candidate {
            id: id.into(),
            entry_type: EntryType::Preference,
            subject: "rust-error-handling".into(),
            created_at: Some("2026-05-12".into()),
            valid_from: None,
            supersedes: None,
            confidence: Some(Confidence::High),
            scope: None,
            source: vec!["sessions/abc.jsonl#turn:2".into()],
            body: "偏好用 thiserror 定义领域错误。".into(),
        }
    }

    #[test]
    fn parse_plain_and_fenced_json() {
        let plain = r#"[{"id":"a","type":"fact","subject":"s","body":"b","source":["x"]}]"#;
        assert_eq!(parse_candidates(plain).unwrap().len(), 1);
        let fenced = format!("blah\n```json\n{plain}\n```\ntrailing");
        assert_eq!(parse_candidates(&fenced).unwrap().len(), 1);
    }

    #[test]
    fn parse_empty_array() {
        assert!(parse_candidates("[]").unwrap().is_empty());
    }

    #[test]
    fn parse_rejects_non_json() {
        assert!(parse_candidates("no array here").is_err());
    }

    #[test]
    fn vet_accepts_valid_candidate() {
        let entry = vet(candidate("pref-rust"), &HashMap::new(), "2026-05-12", None, None).unwrap();
        assert_eq!(entry.id, "pref-rust");
        assert_eq!(entry.status, EntryStatus::Active);
        assert_eq!(entry.source, vec!["sessions/abc.jsonl#turn:2"]);
    }

    #[test]
    fn vet_rejects_bad_slug() {
        let mut c = candidate("Bad Id!");
        c.source = vec!["x".into()];
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_rejects_missing_source_without_session_ref() {
        let mut c = candidate("pref-x");
        c.source.clear();
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_stamps_session_ref_when_source_empty() {
        let mut c = candidate("pref-x");
        c.source.clear();
        let entry = vet(
            c,
            &HashMap::new(),
            "2026-05-12",
            Some("sessions/abc.jsonl"),
            None,
        )
        .unwrap();
        assert_eq!(entry.source, vec!["sessions/abc.jsonl"]);
    }

    #[test]
    fn vet_rejects_bad_date() {
        let mut c = candidate("pref-x");
        c.created_at = Some("2026/05/12".into());
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_rejects_dangling_supersedes() {
        let mut c = candidate("pref-new");
        c.supersedes = Some("pref-old".into());
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, None).is_err());
    }

    #[test]
    fn vet_accepts_existing_supersedes() {
        let old = vet(candidate("pref-old"), &HashMap::new(), "2026-05-12", None, None).unwrap();
        let mut existing = HashMap::new();
        existing.insert(old.id.clone(), old);
        let mut c = candidate("pref-new");
        c.supersedes = Some("pref-old".into());
        assert!(vet(c, &existing, "2026-05-12", None, None).is_ok());
    }

    #[test]
    fn vet_checks_source_file_exists() {
        let dir = std::env::temp_dir().join(format!("shirley_curator_src_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("s.jsonl"), "").unwrap();
        let mut c = candidate("pref-x");
        c.source = vec!["s.jsonl#turn:1".into()];
        assert!(vet(c.clone(), &HashMap::new(), "2026-05-12", None, Some(&dir)).is_ok());
        c.source = vec!["missing.jsonl#turn:1".into()];
        assert!(vet(c, &HashMap::new(), "2026-05-12", None, Some(&dir)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_transcript_includes_user_and_tool() {
        let messages = vec![
            Message::System {
                content: "ignore me".into(),
            },
            Message::User {
                content: "你好".into(),
            },
            Message::Assistant {
                content: Some("在".into()),
                reasoning_content: None,
                tool_calls: vec![],
            },
        ];
        let text = render_transcript(&messages);
        assert!(text.contains("## User"));
        assert!(text.contains("你好"));
        assert!(!text.contains("ignore me"));
    }

    #[test]
    fn build_prompt_mentions_existing_and_boundary() {
        let existing = vec![(
            "preferences/a.md".into(),
            vet(candidate("pref-a"), &HashMap::new(), "2026-05-12", None, None).unwrap(),
        )];
        let prompt = build_prompt("## User\nhi\n", &existing, "sessions/x.jsonl");
        assert!(prompt.contains("部分证据"));
        assert!(prompt.contains("pref-a"));
        assert!(prompt.contains("sessions/x.jsonl#turn:N"));
    }
}
