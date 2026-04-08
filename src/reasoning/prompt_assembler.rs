use crate::{context::Context, snapshot::Snapshot};

use super::{
    prompt_doc::{PromptBlock, PromptDocument, PromptNode, PromptUnitDoc},
    prompt_parts::{
        AppSnapshotPart, AppsSystemPart, CompiledAdditionsSystemPart, EventSystemPart,
        EventsSnapshotPart, MemoriesSnapshotPart, MemoriesSystemPart, PersonaSystemPart,
        PlanSnapshotPart, PlanSystemPart, SensorySnapshotPart, SkillsSystemPart, SnapshotPart,
        SystemPromptPart,
    },
    prompts::{
        APPS_UNIT_HOW, APPS_UNIT_WHAT, APPS_UNIT_WHEN, EVENT_UNIT_HOW, EVENT_UNIT_WHAT,
        MEMORIES_UNIT_HOW, MEMORIES_UNIT_WHAT, MEMORIES_UNIT_WHEN, PLAN_UNIT_HOW, PLAN_UNIT_WHEN,
        PLAN_UNIT_WHAT, SKILLS_UNIT_HOW, SKILLS_UNIT_WHAT, SKILLS_UNIT_WHEN,
    },
    turn_compile::load_prompt_persona_spec_sync,
};

pub struct SystemPromptAssembler {
    parts: Vec<Box<dyn SystemPromptPart>>,
}

pub struct SnapshotAssembler {
    parts: Vec<Box<dyn SnapshotPart>>,
}

impl SystemPromptAssembler {
    pub fn new(parts: Vec<Box<dyn SystemPromptPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(EventSystemPart),
            Box::new(AppsSystemPart),
            Box::new(SkillsSystemPart),
            Box::new(MemoriesSystemPart),
            Box::new(PlanSystemPart),
            Box::new(PersonaSystemPart),
            Box::new(CompiledAdditionsSystemPart),
        ])
    }

    pub fn assemble(&self, ctx: &Context) -> PromptDocument {
        PromptDocument::new(
            self.parts
                .iter()
                .filter_map(|part| part.build(ctx).map(PromptNode::Unit))
                .collect(),
        )
    }
}

impl SnapshotAssembler {
    pub fn new(parts: Vec<Box<dyn SnapshotPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(MemoriesSnapshotPart),
            Box::new(SensorySnapshotPart),
            Box::new(PlanSnapshotPart),
            Box::new(EventsSnapshotPart),
            Box::new(AppSnapshotPart),
        ])
    }

    pub fn assemble(&self, ctx: &Context, snapshot: &Snapshot) -> PromptDocument {
        PromptDocument::new(
            self.parts
                .iter()
                .filter_map(|part| part.build(ctx, snapshot))
                .collect(),
        )
    }
}

pub fn runtime_system_prompt_doc_from_additions(additions: &[String]) -> PromptDocument {
    let persona = load_prompt_persona_spec_sync();
    let mut nodes = vec![
        PromptNode::Unit(PromptUnitDoc::new(
            "event",
            vec![PromptBlock::Paragraph(EVENT_UNIT_WHAT.to_string())],
            Vec::new(),
            Vec::new(),
            vec![PromptBlock::Paragraph(EVENT_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "apps",
            vec![PromptBlock::Paragraph(APPS_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(APPS_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(APPS_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "skills",
            vec![PromptBlock::Paragraph(SKILLS_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(SKILLS_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(SKILLS_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "memories",
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "plan",
            vec![PromptBlock::Paragraph(PLAN_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(PLAN_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(PLAN_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "persona",
            vec![PromptBlock::KeyValueList(vec![
                ("name".to_string(), persona.name.trim().to_string()),
                ("language".to_string(), persona.language.trim().to_string()),
                (
                    "identity_summary".to_string(),
                    persona.identity_summary.trim().to_string(),
                ),
            ])],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )),
    ];
    let additions = additions
        .iter()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if !additions.is_empty() {
        nodes.push(PromptNode::Unit(PromptUnitDoc::new(
            "compiled_additions",
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![PromptBlock::BulletList(additions)],
        )));
    }
    PromptDocument::new(nodes)
}

pub fn baseline_runtime_contract_doc() -> PromptDocument {
    PromptDocument::new(vec![
        PromptNode::Unit(PromptUnitDoc::new(
            "event",
            vec![PromptBlock::Paragraph(EVENT_UNIT_WHAT.to_string())],
            Vec::new(),
            Vec::new(),
            vec![PromptBlock::Paragraph(EVENT_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "apps",
            vec![PromptBlock::Paragraph(APPS_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(APPS_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(APPS_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "skills",
            vec![PromptBlock::Paragraph(SKILLS_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(SKILLS_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(SKILLS_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "memories",
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(MEMORIES_UNIT_HOW.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "plan",
            vec![PromptBlock::Paragraph(PLAN_UNIT_WHAT.to_string())],
            Vec::new(),
            vec![PromptBlock::Paragraph(PLAN_UNIT_WHEN.to_string())],
            vec![PromptBlock::Paragraph(PLAN_UNIT_HOW.to_string())],
        )),
    ])
}
