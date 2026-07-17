#[derive(Clone, Debug, Default)]
pub struct PromptDocument {
    pub nodes: Vec<PromptNode>,
}

#[derive(Clone, Debug)]
pub enum PromptNode {
    State(PromptStateDoc),
    Group(PromptGroupDoc),
}

#[derive(Clone, Debug)]
pub struct PromptStateDoc {
    pub key: String,
    pub blocks: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PromptGroupDoc {
    pub key: String,
    pub children: Vec<PromptNode>,
}

impl PromptDocument {
    pub const fn new(nodes: Vec<PromptNode>) -> Self {
        Self { nodes }
    }
}

impl PromptStateDoc {
    pub fn new(key: impl Into<String>, blocks: Vec<String>) -> Self {
        Self {
            key: key.into(),
            blocks,
        }
    }
}

impl PromptGroupDoc {
    pub fn new(key: impl Into<String>, children: Vec<PromptNode>) -> Self {
        Self {
            key: key.into(),
            children,
        }
    }
}
