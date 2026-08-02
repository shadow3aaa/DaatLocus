local Input = {
  type = "object",
  properties = {
    goals = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "goals" },
  additionalProperties = false,
}

local InvestigatorInput = {
  type = "object",
  properties = {
    goal = { type = "string" },
  },
  required = { "goal" },
  additionalProperties = false,
}

local InvestigatorOutput = {
  type = "object",
  properties = {
    findings = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "findings", "sources" },
  additionalProperties = false,
}

local Investigation = {
  type = "object",
  properties = {
    goal = { type = "string" },
    findings = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "goal", "findings", "sources" },
  additionalProperties = false,
}

local SynthesisInput = {
  type = "object",
  properties = {
    goals = {
      type = "array",
      items = { type = "string" },
    },
    investigations = {
      type = "array",
      items = Investigation,
    },
  },
  required = { "goals", "investigations" },
  additionalProperties = false,
}

local SynthesisOutput = {
  type = "object",
  properties = {
    summary = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "summary", "sources" },
  additionalProperties = false,
}

local Output = {
  type = "object",
  properties = {
    summary = { type = "string" },
    investigations = {
      type = "array",
      items = Investigation,
    },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "summary", "investigations", "sources" },
  additionalProperties = false,
}

local function new_investigator()
  return workflow.agent({
    role = "investigation",
    model = "efficient",
    input = InvestigatorInput,
    output = InvestigatorOutput,
    instruction = [[
Conduct a read-only investigation of the assigned goal. Inspect relevant local
or external state and collect concrete evidence. Do not edit files, change
configuration or external state, run write-capable commands, submit forms, or
otherwise perform implementation work. Distinguish directly observed facts from
inferences, preserve uncertainty or unavailable evidence, and return concise
findings with source identifiers such as paths, URLs, or commands.
]],
    extra_tools = {},
  })
end

local synthesizer = workflow.agent({
  role = "synthesis",
  model = "main",
  input = SynthesisInput,
  output = SynthesisOutput,
  instruction = [[
Synthesize the supplied read-only investigation results into a clear,
evidence-based report for the caller. Do not perform additional investigation or
make changes. Cover every requested goal, resolve or identify conflicts, retain
important uncertainty, and return a deduplicated list of the source identifiers
that support the summary.
]],
  extra_tools = {},
})

local function goals_are_nonempty(goals)
  if #goals == 0 then
    return false
  end
  for _, goal in ipairs(goals) do
    if type(goal) ~= "string" or goal:match("%S") == nil then
      return false
    end
  end
  return true
end

workflow.define({
  description = [[
Investigate one or more read-only targets. An isolated investigator handles each
target concurrently, then a separate agent synthesizes their findings into a
report with source identifiers. Do not use this workflow to make changes;
use workflow__goal for state-changing implementation work.
]],
  input = Input,
  output = Output,
  run = function(input, ctx)
    if not goals_are_nonempty(input.goals) then
      error("investigate requires at least one non-blank goal")
    end

    local handles = {}
    for _, goal in ipairs(input.goals) do
      local investigator = new_investigator()
      table.insert(handles, investigator:run({
        goal = goal,
      }))
    end

    local worker_results = workflow.await_all(handles, "await")
    local investigations = {}
    for index, result in ipairs(worker_results) do
      table.insert(investigations, {
        goal = input.goals[index],
        findings = result.findings,
        sources = result.sources,
      })
    end

    local synthesis = workflow.await(synthesizer:run({
      goals = input.goals,
      investigations = investigations,
    }), "await")

    return {
      summary = synthesis.summary,
      investigations = investigations,
      sources = synthesis.sources,
    }
  end,
})
