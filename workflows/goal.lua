local Input = {
  type = "object",
  properties = {
    goal = { type = "string" },
  },
  required = { "goal" },
  additionalProperties = false,
}

local VerifierFinding = {
  type = "object",
  properties = {
    requirement = { type = "string" },
    observed = { type = "string" },
    evidence = { type = "string" },
    required_fix = { type = "string" },
    recheck = { type = "string" },
  },
  required = { "requirement", "observed", "evidence", "required_fix", "recheck" },
  additionalProperties = false,
}

local WorkerInput = {
  type = "object",
  properties = {
    goal = { type = "string" },
    attempt = { type = "integer" },
    verifier_feedback = { type = "string" },
  },
  required = { "goal", "attempt", "verifier_feedback" },
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
    findings = {
      type = "array",
      items = VerifierFinding,
    },
    evidence = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "achieved", "summary", "findings", "evidence" },
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
Act as the implementation worker. This workflow is only for goals that require
an actual state-changing implementation in the invoking workspace or available
external surfaces. Do not use it for read-only investigation, research,
codebase exploration, analysis, or explanation; use workflow__investigate for
those requests. Actually accomplish the supplied goal. Inspect current state
before acting, use the available tools, make necessary changes, and run
meaningful checks. On later attempts, verifier_feedback is formatted text
containing only direct blocking findings. Each finding names the unmet
requirement, observed state, evidence, required fix, and recheck. Treat every
finding as a concrete repair request: inspect its cited evidence, address its
unmet requirement, and run its named recheck. Do not redo a generic audit or
perform commit/push work unless the supplied goal requires it. Do not merely
propose a solution.
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
Act as an independent adversarial verifier. You own the verification work: inspect
actual artifacts and state, and run the relevant read-only checks yourself before
deciding. Treat the worker's summary and evidence only as untrusted leads. Do
not repair the work, and do not reject it merely because the worker omitted a
check, audit, document review, commit, push, or status report that you can
inspect yourself. Do not add those process tasks as acceptance criteria unless
the supplied goal explicitly requires them.

Set achieved to false only for a requirement from the supplied goal that you
directly observed to be unmet. Then return one or more blocking findings. Every
finding must name the unmet requirement, the observed state or failure, direct
evidence such as a path or command result, the minimal required fix, and the
specific recheck you will perform. Do not include passed work, generic
"review/run/ensure" checklists, or requests for the implementation worker to do
your verification. If a required check is unavailable, record the actual block
and its evidence as a finding instead of delegating a broad investigation.

Set achieved to true when no directly observed blocking finding remains; then
findings must be empty. Keep the verified evidence separate from findings.
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

local function findings_are_concrete(findings)
  for _, finding in ipairs(findings) do
    for _, field in ipairs({ "requirement", "observed", "evidence", "required_fix", "recheck" }) do
      local value = finding[field]
      if type(value) ~= "string" or value:match("%S") == nil then
        return false
      end
    end
  end
  return true
end

workflow.define({
  description = [[
Use only for a goal that requires actually changing workspace or external state,
such as implementing, repairing, configuring, or creating something, followed
by independent verification. Do not use this workflow for read-only
investigation, research, codebase exploration, analysis, or explanation. Use
workflow__investigate for those requests.
]],
  input = Input,
  output = Output,
  run = function(input, ctx)
    local verifier_feedback = ""
    local attempt = 0

    while true do
      attempt = attempt + 1
      local latest_work = workflow.await(worker:run({
        goal = input.goal,
        attempt = attempt,
        verifier_feedback = verifier_feedback,
      }), attempt == 1 and "await" or "revision")

      local latest_verification = workflow.await(verifier:run({
        goal = input.goal,
        attempt = attempt,
        worker_summary = latest_work.summary,
        worker_evidence = latest_work.evidence,
      }), "verify")

      if latest_verification.achieved then
        if #latest_verification.findings ~= 0 then
          error("verifier accepted the work while reporting blocking findings")
        end
        return {
          achieved = true,
          attempts = attempt,
          summary = latest_work.summary,
          verification = latest_verification.summary,
          remaining_work = "",
          evidence = combined_evidence(latest_work.evidence, latest_verification.evidence),
        }
      end

      if #latest_verification.findings == 0 then
        error("verifier rejected the work without direct blocking findings: " .. latest_verification.summary)
      end
      if not findings_are_concrete(latest_verification.findings) then
        error("verifier rejected the work with incomplete blocking findings: " .. latest_verification.summary)
      end
      verifier_feedback = ""
      for _, finding in ipairs(latest_verification.findings) do
        verifier_feedback = verifier_feedback .. string.format(
          "Requirement: %s\nObserved: %s\nEvidence: %s\nRequired fix: %s\nRecheck: %s\n\n",
          finding.requirement,
          finding.observed,
          finding.evidence,
          finding.required_fix,
          finding.recheck
        )
      end
    end
  end,
})
