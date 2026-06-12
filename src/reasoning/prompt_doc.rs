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
    pub blocks: Vec<PromptBlock>,
}

#[derive(Clone, Debug)]
pub struct PromptGroupDoc {
    pub key: String,
    pub children: Vec<PromptNode>,
}

#[derive(Clone, Debug)]
pub enum PromptBlock {
    Paragraph(String),
    BulletList(Vec<String>),
    KeyValueList(Vec<(String, String)>),
}

impl PromptDocument {
    pub fn new(nodes: Vec<PromptNode>) -> Self {
        Self { nodes }
    }
}

impl PromptStateDoc {
    pub fn new(key: impl Into<String>, blocks: Vec<PromptBlock>) -> Self {
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
