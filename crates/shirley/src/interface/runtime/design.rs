//! Creating a bot from what the user says they need.
//!
//! One model call turns the description into a spec (id, name, persona) and a choice of market
//! skills. The bot is saved at once so the user can start talking to it; its skills download in
//! the background and the bot picks them up when they arrive (`reload_bot`).

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use shirley_agent_sdk::Agent;

use super::BotRuntimeManager;
use crate::bot::BotSpec;
use crate::prompt_template::render;
use crate::skills::market::{self, MarketSkill};

const DESIGN_TEMPLATE: &str = include_str!("../../../prompts/design_bot.md");
const DESIGN_TIMEOUT: Duration = Duration::from_secs(90);

/// What the model proposes.
#[derive(Debug, Deserialize)]
struct Proposal {
    name: String,
    display_name: String,
    persona: String,
    #[serde(default)]
    skills: Vec<String>,
}

impl BotRuntimeManager {
    /// Design, save and equip a bot for `brief`. Returns the saved spec; skills are still
    /// installing when it returns.
    pub async fn design_bot(self: &Arc<Self>, brief: &str) -> Result<BotSpec, String> {
        let brief = brief.trim();
        if brief.is_empty() {
            return Err("说说你需要一个怎样的 Bot".to_owned());
        }
        let bots_root = crate::storage::bots_root();
        let taken: Vec<String> = BotSpec::list_all(&bots_root).into_iter().map(|bot| bot.name).collect();
        let catalog = market::index(self.market_index_url().as_deref()).await;

        let config = self.factory.default_model_config().map_err(|error| error.to_string())?;
        let agent = Agent::builder()
            .model_config(config)
            .build()
            .map_err(|error| error.to_string())?;
        let prompt = design_prompt(brief, &catalog, &taken);
        let answer = tokio::time::timeout(DESIGN_TIMEOUT, agent.complete(&prompt))
            .await
            .map_err(|_elapsed| "模型太久没有回应，请再试一次".to_owned())?
            .map_err(|error| error.to_string())?;
        let proposal = parse_proposal(&answer).ok_or_else(|| "没能设计出这个 Bot，请换个说法再试".to_owned())?;

        let spec = BotSpec {
            name: unique_name(&slug(&proposal.name), &taken),
            display_name: proposal.display_name.trim().chars().take(24).collect(),
            tools: Vec::new(),
            // Market skills go into the bot's own directory; the shared ones stay all-allowed.
            skills: Vec::new(),
            system_prompt_extra: Some(proposal.persona.trim().to_owned()),
        };
        if spec.display_name.is_empty() || spec.system_prompt_extra.as_deref() == Some("") {
            return Err("没能设计出这个 Bot，请换个说法再试".to_owned());
        }
        spec.save(&bots_root)?;

        let chosen: Vec<MarketSkill> = catalog
            .into_iter()
            .filter(|skill| proposal.skills.contains(&skill.id))
            .collect();
        if !chosen.is_empty() {
            let manager = Arc::clone(self);
            let bot = spec.name.clone();
            let home = bots_root.join(&bot);
            tokio::spawn(async move {
                for skill in &chosen {
                    if let Err(error) = market::install(&home, skill).await {
                        eprintln!("[market] installing {} into {bot}: {error}", skill.id);
                    }
                }
                manager.reload_bot(&bot).await;
            });
        }
        Ok(spec)
    }

    /// `[market] index_url`, or `SHIRLEY_MARKET_INDEX`.
    pub fn market_index_url(&self) -> Option<String> {
        std::env::var("SHIRLEY_MARKET_INDEX")
            .ok()
            .filter(|url| !url.trim().is_empty())
            .or_else(|| {
                crate::settings::FileSettings::load_file(&self.factory.global_root().join("config.toml"))
                    .ok()
                    .flatten()
                    .and_then(|settings| settings.market.index_url)
            })
    }
}

fn design_prompt(brief: &str, catalog: &[MarketSkill], taken: &[String]) -> String {
    let skills = catalog
        .iter()
        .map(|skill| format!("- {}: {} {}", skill.id, skill.title, skill.summary))
        .collect::<Vec<_>>()
        .join("\n");
    render(
        DESIGN_TEMPLATE,
        &[("brief", brief), ("skills", &skills), ("taken", &taken.join(", "))],
    )
}

/// The JSON object in the answer, fenced or not.
fn parse_proposal(answer: &str) -> Option<Proposal> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    serde_json::from_str(answer.get(start..=end)?).ok()
}

/// A valid bot id from whatever the model wrote.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(24).collect();
    let out = out.trim_end_matches('-').to_owned();
    if BotSpec::is_valid_name(&out) {
        out
    } else {
        format!("bot-{}", &uuid::Uuid::new_v4().simple().to_string()[..6])
    }
}

/// `name`, or `name-2`, `name-3`, ... when it is taken.
fn unique_name(name: &str, taken: &[String]) -> String {
    if !taken.iter().any(|t| t == name) {
        return name.to_owned();
    }
    (2..10_000)
        .map(|n| format!("{name}-{n}"))
        .find(|candidate| !taken.iter().any(|t| t == candidate))
        .unwrap_or_else(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_proposal_is_read_from_a_chatty_or_fenced_answer() {
        let answer = "Here you go:\n```json\n{\"name\": \"Weekly Reporter\", \"display_name\": \"周报\", \
                      \"persona\": \"You are ...\", \"skills\": [\"docx\"]}\n```";
        let proposal = parse_proposal(answer).unwrap();
        assert_eq!(proposal.display_name, "周报");
        assert_eq!(proposal.skills, ["docx"]);
        assert!(parse_proposal("no json").is_none());
    }

    #[test]
    fn names_become_valid_unique_ids() {
        assert_eq!(slug("Weekly Reporter"), "weekly-reporter");
        assert_eq!(slug("  trip_planner!! "), "trip-planner");
        assert!(slug("42").starts_with("bot-") && BotSpec::is_valid_name(&slug("42")));
        assert!(BotSpec::is_valid_name(&slug("周报助手")));
        assert!(BotSpec::is_valid_name(&slug("Data")));
        let taken = vec!["scout".to_owned(), "scout-2".to_owned()];
        assert_eq!(unique_name("scout", &taken), "scout-3");
        assert_eq!(unique_name("clerk", &taken), "clerk");
    }

    #[test]
    fn the_prompt_lists_skills_and_taken_ids() {
        let catalog = vec![MarketSkill {
            id: "pdf".into(),
            title: "PDF 处理".into(),
            summary: "读 PDF".into(),
            category: String::new(),
            repo: "a/b".into(),
            git_ref: "main".into(),
            path: "skills/pdf".into(),
            license: String::new(),
        }];
        let text = design_prompt("帮我整理发票", &catalog, &["nono".to_owned()]);
        assert!(text.contains("- pdf: PDF 处理") && text.contains("帮我整理发票") && text.contains("nono"));
        assert!(!text.contains("{{"));
    }
}
