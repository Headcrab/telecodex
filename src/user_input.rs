//! Native question data and answers, independent of Telegram presentation.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NativeRequestId {
    Number(i64),
    String(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequest {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub is_blocking: bool,
    pub questions: Vec<Question>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    pub options: Option<Vec<QuestionOption>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

impl UserInputRequest {
    pub fn validate(&self) -> Result<()> {
        if self.thread_id.is_empty()
            || self.turn_id.is_empty()
            || self.item_id.is_empty()
            || !(1..=3).contains(&self.questions.len())
        {
            bail!("unsupported native question context or count");
        }
        let mut ids = HashSet::new();
        for question in &self.questions {
            if question.id.is_empty()
                || question.question.trim().is_empty()
                || !ids.insert(&question.id)
                || question.is_secret
            {
                bail!("unsupported native question: empty/duplicate ID or secret input");
            }
            if question.options.as_ref().is_some_and(|options| {
                options.len() > 10 || options.iter().any(|o| o.label.trim().is_empty())
            }) {
                bail!("unsupported native question options");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct QuestionAnswer {
    pub answers: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct UserInputResponse {
    pub answers: BTreeMap<String, QuestionAnswer>,
}

#[derive(Debug)]
pub enum UserInputDecision {
    Answers(UserInputResponse),
    Cancel,
}
