#[derive(Clone, Default)]
pub struct PromptIR {
    pub system: Vec<String>,
    pub instructions: Vec<String>,
    pub sections: Vec<PromptSection>,
}

#[derive(Clone)]
pub struct PromptSection {
    pub title: String,
    pub body: String,
}

impl PromptIR {
    pub fn with_system(system: impl Into<String>) -> Self {
        Self {
            system: vec![system.into()],
            instructions: Vec::new(),
            sections: Vec::new(),
        }
    }

    pub fn push_instruction(&mut self, instruction: impl Into<String>) {
        self.instructions.push(instruction.into());
    }

    pub fn push_section(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.sections.push(PromptSection {
            title: title.into(),
            body: body.into(),
        });
    }
}
