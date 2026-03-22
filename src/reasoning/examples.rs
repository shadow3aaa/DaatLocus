use serde::{Deserialize, Serialize};

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
