local Input = {
  type = "object",
  properties = {
    topic = { type = "string" },
    sources = {
      type = "array",
      items = { type = "string" },
    },
  },
  required = { "topic", "sources" },
  additionalProperties = false,
}

local ResearchOutput = {
  type = "object",
  properties = {
    summary = { type = "string" },
  },
  required = { "summary" },
  additionalProperties = false,
}

local Output = {
  type = "object",
  properties = {
    summary = { type = "string" },
    source_count = { type = "integer" },
  },
  required = { "summary", "source_count" },
  additionalProperties = false,
}

workflow.tool({
  name = "count_sources",
  input = {
    type = "object",
    properties = {
      sources = { type = "array", items = { type = "string" } },
    },
    required = { "sources" },
    additionalProperties = false,
  },
  output = {
    type = "object",
    properties = { count = { type = "integer" } },
    required = { "count" },
    additionalProperties = false,
  },
  run = function(input)
    return { count = #input.sources }
  end,
})

local researcher = workflow.agent({
  model = "efficient",
  input = Input,
  output = ResearchOutput,
  instruction = [[
Write a concise research brief from the supplied topic and source URLs.
Use the count_sources tool when it is useful. Return only the required JSON
object; do not add Markdown.
]],
  extra_tools = { "count_sources" },
})

workflow.define({
  input = Input,
  output = Output,
  run = function(input, ctx)
    local research = workflow.await(researcher:run(input))
    return {
      summary = research.summary,
      source_count = #input.sources,
    }
  end,
})
