use serde::{Deserialize, Serialize};

use crate::tool_ui::{
    PatchFileUiData, PatchUiData, ReplyDisposition, ReplySubject, ReplyUiData, TelegramUiAction,
    TelegramUiData,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchActivityCell {
    pub summary_line: String,
    pub files: Vec<PatchFileUiData>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramActivityCell {
    pub title: String,
    pub detail_lines: Vec<String>,
    pub message_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyActivityCell {
    pub disposition: ReplyDisposition,
    pub subject: ReplySubject,
    pub message_lines: Vec<String>,
}

impl From<PatchUiData> for PatchActivityCell {
    fn from(data: PatchUiData) -> Self {
        PatchActivityCell {
            summary_line: data.summary_line,
            files: data.files,
        }
    }
}

impl From<TelegramUiData> for TelegramActivityCell {
    fn from(data: TelegramUiData) -> Self {
        let mut detail_lines = data.detail_lines;
        if detail_lines.is_empty() {
            detail_lines.push(match data.action {
                TelegramUiAction::ListChats => "list chats".to_string(),
                TelegramUiAction::ReadHistory => "read history".to_string(),
                TelegramUiAction::SelectChat => "select chat".to_string(),
                TelegramUiAction::SendMessage => "send message".to_string(),
                TelegramUiAction::ResolveChat => "resolve chat".to_string(),
            });
        }
        TelegramActivityCell {
            title: data.title,
            detail_lines,
            message_lines: data.message_lines,
        }
    }
}

impl From<ReplyUiData> for ReplyActivityCell {
    fn from(data: ReplyUiData) -> Self {
        ReplyActivityCell {
            disposition: data.disposition,
            subject: data.subject,
            message_lines: data.message_lines,
        }
    }
}
