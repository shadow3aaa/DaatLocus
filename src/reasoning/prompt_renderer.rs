use super::prompt_doc::{PromptBlock, PromptDocument, PromptGroupDoc, PromptNode, PromptStateDoc};

pub struct LlmPromptRenderer;

impl LlmPromptRenderer {
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

    pub fn render_node(node: &PromptNode) -> String {
        match node {
            PromptNode::State(state) => render_state(state),
            PromptNode::Group(group) => render_group(group),
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
