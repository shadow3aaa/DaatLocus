use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[derive(Clone, Serialize, Deserialize)]
pub struct ProgramExample<O> {
    pub title: String,
    pub inputs: Vec<ExampleField>,
    pub output: O,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct ExampleField {
    pub name: String,
    pub value: String,
}

impl<O: Serialize + Clone + DeserializeOwned> ProgramExample<O> {
    pub fn new(title: impl Into<String>, output: O) -> Self {
        Self {
            title: title.into(),
            inputs: Vec::new(),
            output,
        }
    }

    pub fn input(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.inputs.push(ExampleField {
            name: name.into(),
            value: value.into(),
        });
        self
    }
}
