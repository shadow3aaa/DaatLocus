use crate::{context::Context, preturn_state::PreTurnState};

use super::{
    prompt_doc::{PromptBlock, PromptDocument, PromptNode, PromptUnitDoc},
    prompt_parts::{
        AfterClaimContextInput, AfterClaimContextPart, AfterClaimInputPart,
        AfterClaimWorkflowPrimitiveRoutingPart, AppDocsSystemPart, AppsSystemPart,
        CompiledAdditionsSystemPart, EventSystemPart, PersonaSystemPart, PlanSystemPart,
        PreTurnAppSurfacePart, PreTurnContextPart, PreTurnPlanPart, PreTurnSensoryPart,
        PreTurnWorkflowStatePart, SystemPromptPart, WorkflowSystemPart, WorkspaceSystemPart,
    },
    prompts::{
        SYSTEM_APPS, SYSTEM_EVENT, SYSTEM_PLAN, SYSTEM_PRIMITIVE,
        build_workspace_unit_placeholder_prompt,
    },
    turn_compile::load_prompt_persona_spec_sync,
};

pub struct SystemPromptAssembler {
    parts: Vec<Box<dyn SystemPromptPart>>,
}

pub struct PreTurnContextAssembler {
    parts: Vec<Box<dyn PreTurnContextPart>>,
}

pub struct AfterClaimContextAssembler {
    parts: Vec<Box<dyn AfterClaimContextPart>>,
}

impl SystemPromptAssembler {
    pub fn new(parts: Vec<Box<dyn SystemPromptPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(EventSystemPart),
            Box::new(AppsSystemPart),
            Box::new(WorkspaceSystemPart),
            Box::new(PlanSystemPart),
            Box::new(WorkflowSystemPart),
            Box::new(AppDocsSystemPart),
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

impl PreTurnContextAssembler {
    pub fn new(parts: Vec<Box<dyn PreTurnContextPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(PreTurnSensoryPart),
            Box::new(PreTurnPlanPart),
            Box::new(PreTurnWorkflowStatePart),
            Box::new(PreTurnAppSurfacePart),
        ])
    }

    pub fn assemble(&self, ctx: &Context, state: &PreTurnState) -> PromptDocument {
        PromptDocument::new(
            self.parts
                .iter()
                .filter_map(|part| part.build(ctx, state))
                .collect(),
        )
    }
}

impl AfterClaimContextAssembler {
    pub fn new(parts: Vec<Box<dyn AfterClaimContextPart>>) -> Self {
        Self { parts }
    }

    pub fn default_runtime() -> Self {
        Self::new(vec![
            Box::new(AfterClaimInputPart),
            Box::new(AfterClaimWorkflowPrimitiveRoutingPart),
        ])
    }

    pub fn assemble(&self, ctx: &Context, input: &AfterClaimContextInput) -> PromptDocument {
        PromptDocument::new(
            self.parts
                .iter()
                .filter_map(|part| part.build(ctx, input))
                .collect(),
        )
    }
}

pub fn runtime_system_prompt_doc_from_additions(additions: &[String]) -> PromptDocument {
    let persona = load_prompt_persona_spec_sync();
    let mut nodes = vec![
        PromptNode::Unit(PromptUnitDoc::new(
            "event",
            vec![PromptBlock::Paragraph(SYSTEM_EVENT.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "apps",
            vec![PromptBlock::Paragraph(SYSTEM_APPS.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "workspace",
            vec![PromptBlock::Paragraph(
                build_workspace_unit_placeholder_prompt(),
            )],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "plan",
            vec![PromptBlock::Paragraph(SYSTEM_PLAN.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "primitive",
            vec![PromptBlock::Paragraph(SYSTEM_PRIMITIVE.to_string())],
        )),
        PromptNode::Unit(PromptUnitDoc::new(
            "persona",
            vec![PromptBlock::KeyValueList(vec![
                ("name".to_string(), persona.name.trim().to_string()),
                ("language".to_string(), persona.language.trim().to_string()),
                (
                    "configured_locale".to_string(),
                    "injected in live runtime prompt".to_string(),
                ),
                (
                    "identity_summary".to_string(),
                    persona.identity_summary.trim().to_string(),
                ),
            ])],
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
            vec![PromptBlock::BulletList(additions)],
        )));
    }
    PromptDocument::new(nodes)
}
