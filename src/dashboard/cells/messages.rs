use serde::{Deserialize, Serialize};

use crate::activity_event::{
    PatchActivityDescriptor, PatchFileActivityDescriptor, ReplyActivityDescriptor,
    ReplyDisposition, ReplySubject, TelegramActivityAction, TelegramActivityDescriptor,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchActivityData {
    pub summary_line: String,
    pub files: Vec<PatchFileActivityDescriptor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TelegramActivityData {
    pub title: String,
    pub detail_lines: Vec<String>,
    pub message_lines: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyActivityData {
    pub disposition: ReplyDisposition,
    pub subject: ReplySubject,
    pub message_lines: Vec<String>,
    pub elapsed_seconds: Option<u64>,
}

impl From<PatchActivityDescriptor> for PatchActivityData {
    fn from(data: PatchActivityDescriptor) -> Self {
        Self {
            summary_line: data.summary_line,
            files: data.files,
        }
    }
}

impl From<TelegramActivityDescriptor> for TelegramActivityData {
    fn from(data: TelegramActivityDescriptor) -> Self {
        let mut detail_lines = data.detail_lines;
        if detail_lines.is_empty() {
            detail_lines.push(match data.action {
                TelegramActivityAction::ListChats => "list chats".to_string(),
                TelegramActivityAction::ReadHistory => "read history".to_string(),
                TelegramActivityAction::SelectChat => "select chat".to_string(),
                TelegramActivityAction::SendMessage => "send message".to_string(),
                TelegramActivityAction::ResolveChat => "resolve chat".to_string(),
            });
        }
        Self {
            title: data.title,
            detail_lines,
            message_lines: data.message_lines,
        }
    }
}

impl From<ReplyActivityDescriptor> for ReplyActivityData {
    fn from(data: ReplyActivityDescriptor) -> Self {
        Self {
            disposition: data.disposition,
            subject: data.subject,
            message_lines: data.message_lines,
            elapsed_seconds: data.elapsed_seconds,
        }
    }
}
