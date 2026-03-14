#[derive(Clone, Default)]
pub struct Signature {
    pub objective: String,
    pub inputs: Vec<SignatureField>,
    pub outputs: Vec<SignatureField>,
    pub rules: Vec<String>,
}

#[derive(Clone)]
pub struct SignatureField {
    pub name: String,
    pub description: String,
}

impl Signature {
    pub fn new(objective: impl Into<String>) -> Self {
        Self {
            objective: objective.into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            rules: Vec::new(),
        }
    }

    pub fn input(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.inputs.push(SignatureField {
            name: name.into(),
            description: description.into(),
        });
        self
    }

    pub fn output(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        self.outputs.push(SignatureField {
            name: name.into(),
            description: description.into(),
        });
        self
    }

    pub fn rule(mut self, rule: impl Into<String>) -> Self {
        self.rules.push(rule.into());
        self
    }
}
