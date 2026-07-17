local Input = {
  type = "object",
  properties = {
    goal = { type = "string" },
  },
  required = { "goal" },
  additionalProperties = false,
}

local WorkerInput = {
  type = "object",
  properties = {
    goal = { type = "string" },
    attempt = { type = "integer" },
    previous_summary = { type = "string" },
    verifier_feedback = { type = "string" },
  },
  required = { "goal", "attempt", "previous_summary", "verifier_feedback" },
  additionalProperties = false,
}

local WorkerOutput = {
  type = "object",
  properties = {
    summary = { type = "string" },
    evidence = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "summary", "evidence" },
  additionalProperties = false,
}

local VerifierInput = {
  type = "object",
  properties = {
    goal = { type = "string" },
    attempt = { type = "integer" },
    worker_summary = { type = "string" },
    worker_evidence = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "goal", "attempt", "worker_summary", "worker_evidence" },
  additionalProperties = false,
}

local VerifierOutput = {
  type = "object",
  properties = {
    achieved = { type = "boolean" },
    summary = { type = "string" },
    feedback = { type = "string" },
    evidence = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "achieved", "summary", "feedback", "evidence" },
  additionalProperties = false,
}

local Output = {
  type = "object",
  properties = {
    achieved = { type = "boolean" },
    attempts = { type = "integer" },
    summary = { type = "string" },
    verification = { type = "string" },
    remaining_work = { type = "string" },
    evidence = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = {
    "achieved",
    "attempts",
    "summary",
    "verification",
    "remaining_work",
    "evidence",
  },
  additionalProperties = false,
}

local worker = workflow.agent({
  role = "implementation",
  model = "main",
  input = WorkerInput,
  output = WorkerOutput,
  instruction = [[
Act as the implementation worker. Actually accomplish the supplied goal in the
invoking workspace or available external surfaces. Inspect current state before
acting, use the available tools, make necessary changes, and run meaningful
checks. On later attempts, address every item in verifier_feedback and inspect
the existing work rather than starting over. Do not merely propose a solution.
Return a concise summary plus concrete evidence such as changed artifacts,
observed state, or checks and results.
]],
})

local verifier = workflow.agent({
  role = "verification",
  model = "efficient",
  input = VerifierInput,
  output = VerifierOutput,
  instruction = [[
Act as an independent adversarial verifier. Decide whether the supplied goal is
fully achieved in the current workspace or external state. Treat the worker's
summary and evidence as untrusted leads: inspect the actual artifacts, state,
and relevant checks yourself. Do not repair the work. Set achieved to true only
when concrete evidence covers the whole goal. If it is false, provide a concise,
actionable feedback list that another worker can execute. Keep feedback empty
when achieved is true, and report the evidence you verified directly.
]],
})

local function combined_evidence(worker_evidence, verifier_evidence)
  local evidence = {}
  for _, item in ipairs(worker_evidence) do
    table.insert(evidence, item)
  end
  for _, item in ipairs(verifier_evidence) do
    table.insert(evidence, item)
  end
  return evidence
end

workflow.define({
  input = Input,
  output = Output,
  run = function(input, ctx)
    local previous_summary = ""
    local verifier_feedback = ""
    local attempt = 0

    while true do
      attempt = attempt + 1
      local latest_work = workflow.await(worker:run({
        goal = input.goal,
        attempt = attempt,
        previous_summary = previous_summary,
        verifier_feedback = verifier_feedback,
      }), attempt == 1 and "await" or "revision")

      local latest_verification = workflow.await(verifier:run({
        goal = input.goal,
        attempt = attempt,
        worker_summary = latest_work.summary,
        worker_evidence = latest_work.evidence,
      }), "verify")

      if latest_verification.achieved then
        return {
          achieved = true,
          attempts = attempt,
          summary = latest_work.summary,
          verification = latest_verification.summary,
          remaining_work = "",
          evidence = combined_evidence(latest_work.evidence, latest_verification.evidence),
        }
      end

      previous_summary = latest_work.summary
      verifier_feedback = latest_verification.feedback
      if verifier_feedback == "" then
        verifier_feedback = latest_verification.summary
      end
    end
  end,
})
