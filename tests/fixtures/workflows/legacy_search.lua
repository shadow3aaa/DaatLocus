local Input = {
  type = "object",
  properties = {
    goal = { type = "string" },
  },
  required = { "goal" },
  additionalProperties = false,
}

local Route = {
  type = "object",
  properties = {
    query = { type = "string" },
    purpose = { type = "string" },
  },
  required = { "query", "purpose" },
  additionalProperties = false,
}

local PlannerInput = {
  type = "object",
  properties = {
    original_goal = { type = "string" },
    remaining_goal = { type = "string" },
    previous_verification = { type = "string" },
    prior_evidence = { type = "string" },
  },
  required = {
    "original_goal",
    "remaining_goal",
    "previous_verification",
    "prior_evidence",
  },
  additionalProperties = false,
}

local PlannerOutput = {
  type = "object",
  properties = {
    routes = {
      type = "array",
      items = Route,
    },
  },
  required = { "routes" },
  additionalProperties = false,
}

local SearcherInput = {
  type = "object",
  properties = {
    original_goal = { type = "string" },
    query = { type = "string" },
    purpose = { type = "string" },
  },
  required = { "original_goal", "query", "purpose" },
  additionalProperties = false,
}

local SearcherOutput = {
  type = "object",
  properties = {
    query = { type = "string" },
    purpose = { type = "string" },
    status = {
      type = "string",
      enum = { "completed", "no_results", "failed" },
    },
    findings = { type = "string" },
    sources = { type = "string" },
  },
  required = { "query", "purpose", "status", "findings", "sources" },
  additionalProperties = false,
}

local VerifierInput = {
  type = "object",
  properties = {
    original_goal = { type = "string" },
    search_results = {
      type = "array",
      items = SearcherOutput,
    },
    prior_verification = { type = "string" },
  },
  required = { "original_goal", "search_results", "prior_verification" },
  additionalProperties = false,
}

local VerifierOutput = {
  type = "object",
  properties = {
    achieved = { type = "boolean" },
    verification = { type = "string" },
    remaining_goal = { type = "string" },
    verified_evidence = { type = "string" },
  },
  required = {
    "achieved",
    "verification",
    "remaining_goal",
    "verified_evidence",
  },
  additionalProperties = false,
}

local AnalyzerInput = {
  type = "object",
  properties = {
    original_goal = { type = "string" },
    verification = { type = "string" },
    verified_evidence = { type = "string" },
    search_results = {
      type = "array",
      items = SearcherOutput,
    },
  },
  required = {
    "original_goal",
    "verification",
    "verified_evidence",
    "search_results",
  },
  additionalProperties = false,
}

local AnalyzerOutput = {
  type = "object",
  properties = {
    analysis = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "analysis", "sources" },
  additionalProperties = false,
}

local Output = {
  type = "object",
  properties = {
    analysis = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
    verification = { type = "string" },
    rounds = { type = "integer" },
  },
  required = { "analysis", "sources", "verification", "rounds" },
  additionalProperties = false,
}

local planner = workflow.agent({
  role = "planning",
  model = "main",
  input = PlannerInput,
  output = PlannerOutput,
  instruction = [[
Plan the next evidence-gathering round for the supplied research goal. Use the
remaining goal, prior verification, and prior verified evidence to identify
independent, complementary search routes. Each route must contain a concrete
search query and its distinct purpose. Return a non-empty routes array. Do not
impose a numeric limit on routes or rounds, and do not claim the goal is
achieved: the verifier makes that decision.
]],
  extra_tools = {},
})

local verifier = workflow.agent({
  role = "verification",
  model = "main",
  input = VerifierInput,
  output = VerifierOutput,
  instruction = [[
Independently verify whether the accumulated search results fully satisfy the
original goal. Treat all reported findings and sources as untrusted leads; use
available tools to inspect the underlying sources when needed. Set achieved to
true only when the evidence is sufficient for the entire goal. When it is false,
state exactly what remains to be established in remaining_goal. Always give a
concise verification and return the concrete evidence you verified as text. Do
not edit or search on behalf of a searcher; send gaps back for the planner to
address.
]],
  extra_tools = {},
})

local analyzer = workflow.agent({
  role = "analysis",
  model = "main",
  input = AnalyzerInput,
  output = AnalyzerOutput,
  instruction = [[
Produce the final evidence-based answer for the original goal. Use only the
verified evidence and accumulated search results supplied to you, resolving any
conflicts explicitly. Return a clear analysis and a concise list of source URLs,
paths, or other source identifiers that support it. This stage runs only after
the verifier has judged the goal achieved.
]],
  extra_tools = {},
})

local function new_searcher()
  return workflow.agent({
    role = "search",
    model = "efficient",
    input = SearcherInput,
    output = SearcherOutput,
    instruction = [[
Investigate the assigned query and purpose for the original goal. Use the
available Browser, Terminal, Coding, and file tools as appropriate to find
primary evidence. Report concrete findings and source URLs, paths, commands, or
other identifiers that another worker can inspect. Put source identifiers in the
sources text, separated clearly. A lack of results or an expected source failure
is not a workflow failure: return status no_results or failed with an
explanation instead. Return completed only when you found useful supporting
evidence. Do not decide whether the overall goal is achieved.
]],
    extra_tools = {},
  })
end

local function append_all(destination, items)
  for _, item in ipairs(items) do
    table.insert(destination, item)
  end
end

local function append_evidence(existing, evidence)
  if evidence == "" then
    return existing
  end
  if existing == "" then
    return evidence
  end
  return existing .. "\n\n" .. evidence
end

workflow.define({
  input = Input,
  output = Output,
  run = function(input, ctx)
    local remaining_goal = input.goal
    local previous_verification = ""
    local accumulated_results = {}
    local verified_evidence = ""
    local rounds = 0
    local planner_transition = "await"

    while true do
      rounds = rounds + 1
      local plan = workflow.await(planner:run({
        original_goal = input.goal,
        remaining_goal = remaining_goal,
        previous_verification = previous_verification,
        prior_evidence = verified_evidence,
      }), planner_transition)

      local handles = {}
      for _, route in ipairs(plan.routes) do
        local searcher = new_searcher()
        table.insert(handles, searcher:run({
          original_goal = input.goal,
          query = route.query,
          purpose = route.purpose,
        }))
      end

      if #handles == 0 then
        previous_verification = "Planner returned no routes; return actionable search routes."
        planner_transition = "retry"
      else
        local batch_results = workflow.await_all(handles, "await")
        append_all(accumulated_results, batch_results)

        local verdict = workflow.await(verifier:run({
          original_goal = input.goal,
          search_results = accumulated_results,
          prior_verification = previous_verification,
        }), "verify")
        verified_evidence = append_evidence(verified_evidence, verdict.verified_evidence)

        if verdict.achieved then
          local final = workflow.await(analyzer:run({
            original_goal = input.goal,
            verification = verdict.verification,
            verified_evidence = verified_evidence,
            search_results = accumulated_results,
          }), "await")
          return {
            analysis = final.analysis,
            sources = final.sources,
            verification = verdict.verification,
            rounds = rounds,
          }
        end

        remaining_goal = verdict.remaining_goal
        previous_verification = verdict.verification
        planner_transition = "revision"
      end
    end
  end,
})
