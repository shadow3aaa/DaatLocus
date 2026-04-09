use super::prompt_doc::{
    PromptBlock, PromptDocument, PromptGroupDoc, PromptNode, PromptStateDoc, PromptUnitDoc,
};

pub struct LlmPromptRenderer;
pub struct DashboardPromptRenderer;

impl LlmPromptRenderer {
    pub fn render_document(doc: &PromptDocument) -> String {
        Self::render_document_with_root(doc, None)
    }

    pub fn render_document_with_root(doc: &PromptDocument, root_tag: Option<&str>) -> String {
        let body = doc
            .nodes
            .iter()
            .map(Self::render_node)
            .filter(|node| !node.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        match root_tag {
            Some(tag) if !body.trim().is_empty() => format!("<{tag}>\n{body}\n</{tag}>"),
            _ => body,
        }
    }

    pub fn render_system_messages(doc: &PromptDocument) -> Vec<String> {
        doc.nodes
            .iter()
            .map(Self::render_node)
            .filter(|node| !node.trim().is_empty())
            .collect()
    }

    pub fn render_node(node: &PromptNode) -> String {
        match node {
            PromptNode::Unit(unit) => render_unit(unit),
            PromptNode::State(state) => render_state(state),
            PromptNode::Group(group) => render_group(group),
        }
    }
}

impl DashboardPromptRenderer {
    pub fn render_document(doc: &PromptDocument, heading: &str) -> String {
        let body = LlmPromptRenderer::render_document_with_root(doc, Some("system_prompt"));
        if body.trim().is_empty() {
            heading.to_string()
        } else {
            format!("{heading}\n\n{body}")
        }
    }
}

fn render_group(group: &PromptGroupDoc) -> String {
    let body = group
        .children
        .iter()
        .map(LlmPromptRenderer::render_node)
        .filter(|child| !child.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if body.trim().is_empty() {
        return String::new();
    }
    format!("<{}>\n{}\n</{}>", group.key, body, group.key)
}

fn render_unit(unit: &PromptUnitDoc) -> String {
    let mut parts = Vec::new();
    if !unit.what.is_empty() {
        let body = render_blocks(&unit.what);
        if !body.trim().is_empty() {
            parts.push(format!("<what>\n{body}\n</what>"));
        }
    }
    if !unit.why.is_empty() {
        let body = render_blocks(&unit.why);
        if !body.trim().is_empty() {
            parts.push(format!("<why>\n{body}\n</why>"));
        }
    }
    if !unit.when.is_empty() {
        let body = render_blocks(&unit.when);
        if !body.trim().is_empty() {
            parts.push(format!("<when>\n{body}\n</when>"));
        }
    }
    if !unit.how.is_empty() {
        let body = render_blocks(&unit.how);
        if !body.trim().is_empty() {
            parts.push(format!("<how>\n{body}\n</how>"));
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("<{}>\n{}\n</{}>", unit.key, parts.join("\n\n"), unit.key)
}

fn render_state(state: &PromptStateDoc) -> String {
    let body = render_blocks(&state.blocks);
    if body.trim().is_empty() {
        return String::new();
    }
    format!("<{}>\n{}\n</{}>", state.key, body, state.key)
}

fn render_blocks(blocks: &[PromptBlock]) -> String {
    blocks
        .iter()
        .map(render_block)
        .filter(|block| !block.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn render_block(block: &PromptBlock) -> String {
    match block {
        PromptBlock::Paragraph(text) => text.clone(),
        PromptBlock::BulletList(items) => items
            .iter()
            .filter(|item| !item.trim().is_empty())
            .map(|item| format!("- {item}"))
            .collect::<Vec<_>>()
            .join("\n"),
        PromptBlock::KeyValueList(items) => items
            .iter()
            .filter(|(key, value)| !key.trim().is_empty() && !value.trim().is_empty())
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}
