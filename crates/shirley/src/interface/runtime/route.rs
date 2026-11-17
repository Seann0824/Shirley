//! Choosing which bot runs a task dropped on the home screen.
//!
//! The user does not pick a bot: one short model call reads the task against each bot's persona
//! and tools and names one. Anything that goes wrong (no model, a timeout, an answer that is not
//! a bot) falls back to the generalist, so a task is never refused for want of a router.

use std::time::Duration;

use shirley_agent_sdk::Agent;

use super::BotRuntimeManager;
use crate::bot::BotSpec;
use crate::prompt_template::render;

const ROUTE_TEMPLATE: &str = include_str!("../../../prompts/route.md");
const ROUTE_TIMEOUT: Duration = Duration::from_secs(20);
/// The generalist, chosen when nothing else clearly fits.
const FALLBACK_BOT: &str = "nono";

impl BotRuntimeManager {
    /// The bot that should run `task`.
    pub async fn route(&self, task: &str) -> String {
        let bots = BotSpec::list_all(&crate::storage::bots_root());
        let fallback = if bots.iter().any(|bot| bot.name == FALLBACK_BOT) {
            FALLBACK_BOT.to_owned()
        } else {
            bots.first()
                .map_or_else(|| FALLBACK_BOT.to_owned(), |bot| bot.name.clone())
        };
        if bots.len() < 2 {
            return fallback;
        }
        let Ok(config) = self.factory.default_model_config() else {
            return fallback;
        };
        let Ok(agent) = Agent::builder().model_config(config).build() else {
            return fallback;
        };
        let prompt = route_prompt(&bots, task, &fallback);
        match tokio::time::timeout(ROUTE_TIMEOUT, agent.complete(&prompt)).await {
            Ok(Ok(answer)) => pick(&bots, &answer).unwrap_or(fallback),
            _ => fallback,
        }
    }
}

fn route_prompt(bots: &[BotSpec], task: &str, fallback: &str) -> String {
    let listing = bots
        .iter()
        .map(|bot| {
            let persona = bot
                .system_prompt_extra
                .as_deref()
                .map_or("A general assistant.", str::trim);
            format!(
                "- {}: {} -- {} (tools: {})",
                bot.name,
                bot.display_name,
                persona.lines().next().unwrap_or_default(),
                bot.effective_tools().join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    render(
        ROUTE_TEMPLATE,
        &[
            ("bots", &listing),
            ("task", task.trim()),
            ("fallback", fallback),
        ],
    )
}

/// The bot an answer names. Exact id first; otherwise the longest id the answer mentions, so a
/// chatty "I'd pick scout." still lands and `nono` is not matched inside a longer id.
fn pick(bots: &[BotSpec], answer: &str) -> Option<String> {
    let answer = answer
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
    let lower = answer.to_ascii_lowercase();
    if let Some(bot) = bots.iter().find(|bot| bot.name == lower) {
        return Some(bot.name.clone());
    }
    bots.iter()
        .filter(|bot| lower.contains(&bot.name) || answer.contains(&bot.display_name))
        .max_by_key(|bot| bot.name.len())
        .map(|bot| bot.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bots() -> Vec<BotSpec> {
        BotSpec::platform_defaults()
    }

    #[test]
    fn picks_an_exact_or_mentioned_bot() {
        assert_eq!(pick(&bots(), "scout").as_deref(), Some("scout"));
        assert_eq!(pick(&bots(), " Patch.\n").as_deref(), Some("patch"));
        assert_eq!(
            pick(&bots(), "I would pick scout here").as_deref(),
            Some("scout")
        );
        assert_eq!(pick(&bots(), "nobody"), None);
    }

    #[test]
    fn prompt_lists_every_bot_with_its_tools() {
        let text = route_prompt(&bots(), "查一下明天成都的天气", "nono");
        for bot in bots() {
            assert!(text.contains(&format!("- {}:", bot.name)), "{text}");
        }
        assert!(text.contains("web_search"), "{text}");
        assert!(text.contains("查一下明天成都的天气"), "{text}");
    }
}
