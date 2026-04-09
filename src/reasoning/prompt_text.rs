#[derive(Default)]
pub struct PromptTextBuilder {
    blocks: Vec<String>,
}

impl PromptTextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_paragraph(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.trim().is_empty() {
            self.blocks.push(text);
        }
    }

    pub fn push_labeled_section(&mut self, title: impl Into<String>, body: impl Into<String>) {
        let title = title.into();
        let body = body.into();
        if body.trim().is_empty() {
            return;
        }
        self.blocks.push(format!("{title}：\n{body}"));
    }

    pub fn push_markdown_section(&mut self, title: impl Into<String>, body: impl Into<String>) {
        let title = title.into();
        let body = body.into();
        if body.trim().is_empty() {
            return;
        }
        self.blocks.push(format!("## {title}\n{body}"));
    }

    pub fn push_bullet_list_section<I, S>(&mut self, title: impl Into<String>, items: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let body = render_bullet_list(items);
        self.push_labeled_section(title, body);
    }

    pub fn build(self) -> String {
        self.blocks.join("\n\n")
    }
}

pub fn render_bullet_list<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    items
        .into_iter()
        .map(Into::into)
        .filter(|item| !item.trim().is_empty())
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}
