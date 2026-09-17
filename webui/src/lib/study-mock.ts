import type {
  DashboardSnapshot,
  StudyEdge,
  StudyGraphSnapshot,
  StudyModule,
  StudyModuleSummary,
  StudyNeighbor,
  StudyNode,
  StudyNodeDetail,
  StudyNodeSummary,
  StudyProgress,
  StudyQuestion,
  StudySource,
  StudyStats,
} from "@/lib/daemon-api";

export const MOCK_STUDY_SESSION_ID = "mock-study-session";

const MOCK_STUDY_NOW_MS = 1_786_800_000_000;

const ALGEBRA_MODULE_ID = "module-algebra";
const CALCULUS_MODULE_ID = "module-calculus";

type MockStudyNodeSeed = {
  id: string;
  moduleId: string;
  title: string;
  summary: string;
  understanding: number;
  aliases: string[];
  tags: string[];
  contentVersion: number;
  staleQuestionCount: number;
};

const MOCK_STUDY_MODULES: StudyModule[] = [
  {
    id: ALGEBRA_MODULE_ID,
    title: "Algebra Foundations",
    description:
      "Number sense, fractions, powers, and the algebra that builds on them.",
    created_at_ms: MOCK_STUDY_NOW_MS - 90 * 24 * 60 * 60 * 1000,
    updated_at_ms: MOCK_STUDY_NOW_MS - 3 * 24 * 60 * 60 * 1000,
  },
  {
    id: CALCULUS_MODULE_ID,
    title: "Calculus",
    description:
      "Functions, limits, derivatives, integrals, and series intuition.",
    created_at_ms: MOCK_STUDY_NOW_MS - 60 * 24 * 60 * 60 * 1000,
    updated_at_ms: MOCK_STUDY_NOW_MS - 1 * 24 * 60 * 60 * 1000,
  },
];

const MOCK_STUDY_NODE_SEEDS: MockStudyNodeSeed[] = [
  {
    id: "numbers",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Numbers and Operations",
    summary:
      "Ordering, arithmetic laws, and the structure that later algebra depends on.",
    understanding: 92,
    aliases: ["arithmetic", "number line"],
    tags: ["basics"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "fractions",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Fractions and Ratios",
    summary:
      "Equivalent fractions, proportion reasoning, and moving fluently between ratio forms.",
    understanding: 38,
    aliases: ["ratio reasoning"],
    tags: ["basics", "proportion"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "exponents",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Exponents and Roots",
    summary:
      "Power laws, negative exponents, and inverse relationships to roots.",
    understanding: 92,
    aliases: ["powers", "radicals"],
    tags: ["basics"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "polynomials",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Polynomials",
    summary:
      "Terms, degree, and the algebraic moves used to rewrite polynomial expressions.",
    understanding: 38,
    aliases: ["polynomial expressions"],
    tags: ["algebra", "expressions"],
    contentVersion: 2,
    staleQuestionCount: 0,
  },
  {
    id: "factoring",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Factoring",
    summary:
      "Turning sums into products and choosing the factoring pattern that fits a polynomial.",
    understanding: 0,
    aliases: ["factorisation"],
    tags: ["algebra", "patterns"],
    contentVersion: 1,
    staleQuestionCount: 1,
  },
  {
    id: "linear-equations",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Linear Equations",
    summary:
      "Slope, intercepts, and solving equations that describe straight-line relationships.",
    understanding: 92,
    aliases: ["lines", "slope"],
    tags: ["algebra", "equations"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "quadratic-equations",
    moduleId: ALGEBRA_MODULE_ID,
    title: "Quadratic Equations",
    summary:
      "Vertex, discriminant, and choosing between factoring and the quadratic formula.",
    understanding: 65,
    aliases: ["quadratics"],
    tags: ["algebra", "equations"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "functions",
    moduleId: CALCULUS_MODULE_ID,
    title: "Functions",
    summary:
      "Domain, range, composition, and reading a function through its graph.",
    understanding: 92,
    aliases: ["mappings"],
    tags: ["calculus", "foundations"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "limits",
    moduleId: CALCULUS_MODULE_ID,
    title: "Limits",
    summary:
      "Approaching a value without reaching it, and the limit laws used to reason about change.",
    understanding: 38,
    aliases: ["limit laws"],
    tags: ["calculus", "analysis"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "derivatives",
    moduleId: CALCULUS_MODULE_ID,
    title: "Derivatives",
    summary:
      "Instantaneous rate of change, tangent lines, and differentiation rules.",
    understanding: 38,
    aliases: ["differentiation"],
    tags: ["calculus", "change"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "integrals",
    moduleId: CALCULUS_MODULE_ID,
    title: "Integrals",
    summary:
      "Accumulation, area under a curve, and the fundamental theorem of calculus.",
    understanding: 0,
    aliases: ["integration", "antiderivatives"],
    tags: ["calculus", "accumulation"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
  {
    id: "series",
    moduleId: CALCULUS_MODULE_ID,
    title: "Series",
    summary:
      "Infinite sums, convergence tests, and approximating functions with polynomials.",
    understanding: 0,
    aliases: ["sequences and series"],
    tags: ["calculus", "approximation"],
    contentVersion: 1,
    staleQuestionCount: 0,
  },
];

const MOCK_STUDY_EDGE_SEEDS: Array<{
  from: string;
  to: string;
  relation: StudyEdge["relation"];
  note: string;
}> = [
  { from: "numbers", to: "fractions", relation: "prerequisite", note: "Arithmetic laws come before ratio reasoning." },
  { from: "numbers", to: "exponents", relation: "prerequisite", note: "Repeated multiplication first needs multiplication." },
  { from: "fractions", to: "polynomials", relation: "prerequisite", note: "Fraction arithmetic appears inside polynomial coefficients." },
  { from: "exponents", to: "polynomials", relation: "prerequisite", note: "Powers define polynomial degree." },
  { from: "polynomials", to: "factoring", relation: "prerequisite", note: "Factoring rewrites polynomial structure." },
  { from: "fractions", to: "linear-equations", relation: "prerequisite", note: "Clearing denominators is a core solving move." },
  { from: "linear-equations", to: "quadratic-equations", relation: "prerequisite", note: "Quadratic methods reduce to linear steps." },
  { from: "factoring", to: "quadratic-equations", relation: "prerequisite", note: "The zero-product rule depends on factoring." },
  { from: "functions", to: "limits", relation: "prerequisite", note: "Limits describe function behavior near a point." },
  { from: "limits", to: "derivatives", relation: "prerequisite", note: "A derivative is defined as a limit." },
  { from: "derivatives", to: "integrals", relation: "prerequisite", note: "The fundamental theorem connects the two." },
  { from: "limits", to: "series", relation: "prerequisite", note: "Convergence is a limit statement." },
  { from: "numbers", to: "functions", relation: "related", note: "Both translate structure into symbolic rules." },
  { from: "derivatives", to: "linear-equations", relation: "related", note: "Tangents are local linear models." },
  { from: "derivatives", to: "quadratic-equations", relation: "applies_to", note: "Optimization problems become quadratic equations." },
  { from: "factoring", to: "integrals", relation: "contrast", note: "Products out of sums versus sums of products." },
  { from: "numbers", to: "limits", relation: "related", note: "Both reason about approximation and order." },
];

function buildMockNodeSummary(seed: MockStudyNodeSeed): StudyNodeSummary {
  return {
    id: seed.id,
    module_id: seed.moduleId,
    title: seed.title,
    summary: seed.summary,
    aliases: [...seed.aliases],
    tags: [...seed.tags],
    content_version: seed.contentVersion,
    progress: mockProgressForSeed(seed),
    question_count: 2,
    stale_question_count: seed.staleQuestionCount,
  };
}

function mockProgressForSeed(seed: MockStudyNodeSeed): StudyProgress {
  return {
    understanding: seed.understanding,
    evidence:
      seed.understanding === 0
        ? ""
        : seed.understanding >= 90
          ? "Completed two graded practice sets."
          : seed.understanding >= 60
            ? "First attempt was mixed; scheduled for review."
            : "Studied with worked examples.",
    updated_by: seed.understanding === 0 ? "code" : "agent",
    updated_at_ms:
      seed.understanding === 0
        ? 0
        : MOCK_STUDY_NOW_MS - 48 * 60 * 60 * 1000,
  };
}

function buildMockEdges(): StudyEdge[] {
  return MOCK_STUDY_EDGE_SEEDS.map((seed, index) => ({
    id: index + 1,
    from: seed.from,
    to: seed.to,
    relation: seed.relation,
    note: seed.note,
  }));
}

function buildMockModules(nodes: StudyNodeSummary[]): StudyModuleSummary[] {
  return MOCK_STUDY_MODULES.map((module) => {
    const moduleNodes = nodes.filter((node) => node.module_id === module.id);
    const understandingTotal = moduleNodes.reduce(
      (total, node) => total + node.progress.understanding,
      0,
    );
    return {
      module: { ...module },
      node_count: moduleNodes.length,
      mastered_count: moduleNodes.filter(
        (node) => node.progress.understanding >= 90,
      ).length,
      in_progress_count: moduleNodes.filter(
        (node) =>
          node.progress.understanding > 0 && node.progress.understanding < 90,
      ).length,
      unseen_count: moduleNodes.filter(
        (node) => node.progress.understanding === 0,
      ).length,
      average_understanding:
        moduleNodes.length === 0
          ? 0
          : Math.round(understandingTotal / moduleNodes.length),
    };
  });
}

function buildMockStats(nodes: StudyNodeSummary[], edges: StudyEdge[]): StudyStats {
  const understandingTotal = nodes.reduce(
    (total, node) => total + node.progress.understanding,
    0,
  );
  return {
    module_count: MOCK_STUDY_MODULES.length,
    node_count: nodes.length,
    edge_count: edges.length,
    mastered_count: nodes.filter((node) => node.progress.understanding >= 90)
      .length,
    in_progress_count: nodes.filter(
      (node) => node.progress.understanding > 0 && node.progress.understanding < 90,
    ).length,
    unseen_count: nodes.filter((node) => node.progress.understanding === 0)
      .length,
    average_understanding:
      nodes.length === 0 ? 0 : Math.round(understandingTotal / nodes.length),
    question_count: nodes.reduce((total, node) => total + node.question_count, 0),
    stale_question_count: nodes.reduce(
      (total, node) => total + node.stale_question_count,
      0,
    ),
    attempt_count: 9,
  };
}

function buildMockGraph(): StudyGraphSnapshot {
  const nodes = MOCK_STUDY_NODE_SEEDS.map(buildMockNodeSummary);
  const edges = buildMockEdges();
  return {
    generated_at_ms: MOCK_STUDY_NOW_MS,
    modules: buildMockModules(nodes),
    nodes,
    edges,
    stats: buildMockStats(nodes, edges),
    maintenance: {
      orphan_node_ids: [],
      empty_module_ids: [],
      duplicate_candidate_groups: [],
      unlinked_node_ids: [],
      stale_question_count: nodes.reduce(
        (total, node) => total + node.stale_question_count,
        0,
      ),
    },
  };
}

export const MOCK_STUDY_GRAPH: StudyGraphSnapshot = buildMockGraph();

const MOCK_STUDY_TOKEN_USAGE = {
  input_tokens: 38_400,
  cached_input_tokens: 24_000,
  output_tokens: 1_800,
  reasoning_output_tokens: 900,
  total_tokens: 41_100,
};

export const MOCK_STUDY_DASHBOARD_SNAPSHOT: DashboardSnapshot = {
  agent_name: "Daat Locus",
  session_title: {
    title: "Study",
    generated: true,
    updated_at_ms: MOCK_STUDY_NOW_MS,
  },
  status_output: "",
  status_command: {
    runtime_turn: "idle",
    bound_primitive: "",
    active_plans: 0,
    events: "0 active",
    plan_steps: [
      {
        status: "completed",
        step: "Review the polynomial node and its relations",
      },
      { status: "completed", step: "Add the missing follow-up concept" },
      {
        status: "in_progress",
        step: "Verify the prerequisite chain stays acyclic",
      },
      { status: "pending", step: "Record the updated understanding level" },
    ],
  },
  sleep_status_output: "",
  inspect_telegram_output: "",
  system_prompt_output: "",
  preturn_context_output: "",
  app_status_outputs: [
    [
      "study",
      [
        "kind=study",
        "modules=2 nodes=12 edges=17",
        "understanding avg=46% mastered=4 in_progress=4 unseen=4",
        "questions=24 stale_questions=1 attempts=9",
      ].join("\n"),
    ],
  ],
  skills: [],
  skill_errors: [],
  workflows: [],
  workflow_errors: [],
  pending_access_requests: [],
  pending_user_inputs: [],
  activity_events: [
    {
      User: {
        content: "Tidy up the Polynomials area and tell me where I stand.",
      },
    },
    {
      Thinking: {
        content:
          "The user wants the polynomial module organized and an understanding check. Read the current node and its neighbors first, then add the missing follow-up concept and record progress.",
      },
    },
    {
      PlanResult: {
        steps: [
          {
            status: "Completed",
            text: "Review the polynomial node and its relations",
          },
          {
            status: "Completed",
            text: "Add the missing follow-up concept",
          },
          {
            status: "InProgress",
            text: "Verify the prerequisite chain stays acyclic",
          },
          {
            status: "Pending",
            text: "Record the updated understanding level",
          },
        ],
      },
    },
    {
      Reply: {
        disposition: "resolved",
        subject: "message",
        message_lines: [
          "Polynomials is now organized: Fractions and Ratios and Exponents and Roots feed into it, and it opens the way to Factoring.",
          "I added Polynomial Division as the next step and raised your recorded understanding to 45% from your last two attempts.",
          "Next: work through two polynomial long-division examples, then I can raise the level again.",
        ],
        elapsed_seconds: 42,
      },
    },
  ],
  live_activity_events: [],
  active_workflow_runs: [],
  last_cycle_elapsed_ms: null,
  runtime_status: "Idle",
  runtime_status_level: "info",
  runtime_activity: {
    status: "idle",
    label: "Idle",
    detail: null,
    active_runtime_turn: false,
    active_runtime_phase: null,
  },
  current_plan_step: {
    status: "in_progress",
    step: "Verify the prerequisite chain stays acyclic",
  },
  token_usage: {
    main: {
      total_token_usage: MOCK_STUDY_TOKEN_USAGE,
      last_token_usage: MOCK_STUDY_TOKEN_USAGE,
      model_context_window: 200_000,
      daily_token_usage: [
        { date: "2026-09-16", usage: MOCK_STUDY_TOKEN_USAGE },
      ],
    },
    main_model: "gpt-5.5",
    judge: null,
    judge_model: null,
    efficient_model: "gpt-5.5",
  },
  footer_context: "gpt-5.5 · Study · 41k/200k used",
  footer_estimated_input_tokens: 41_100,
  context_composition: {
    captured_at_ms: MOCK_STUDY_NOW_MS,
    model: "gpt-5.5",
    model_context_window: 200_000,
    total_estimated_tokens: 41_100,
    total_bytes: 164_400,
    message_count: 18,
    tool_count: 14,
    tools_schema_tokens: 9_800,
    stable_prefix_tokens: 22_400,
    new_suffix_tokens: 12_100,
    changed_prefix_tokens: 6_600,
    previous_common_prefix_tokens: 20_100,
    previous_request_hash: "mock-study-request-previous",
    current_request_hash: "mock-study-request-current",
    segments: [
      {
        name: "system_messages",
        label: "System messages",
        source: "system",
        tokens: 7_200,
        bytes: 28_800,
        percent: 17.5,
        hash: "mock-study-system",
        cache_role: "prefix",
      },
      {
        name: "afterclaim_context",
        label: "Afterclaim context",
        source: "user",
        tokens: 4_100,
        bytes: 16_400,
        percent: 10,
        hash: "mock-study-afterclaim",
        cache_role: "history",
      },
      {
        name: "preturn_context",
        label: "Preturn context",
        source: "user",
        tokens: 8_300,
        bytes: 33_200,
        percent: 20.2,
        hash: "mock-study-preturn",
        cache_role: "history",
      },
      {
        name: "conversation_history",
        label: "Conversation history",
        source: "user",
        tokens: 11_900,
        bytes: 47_600,
        percent: 28.9,
        hash: "mock-study-history",
        cache_role: "history",
      },
      {
        name: "tools_schema",
        label: "Tools schema",
        source: "request_tools",
        tokens: 9_800,
        bytes: 39_200,
        percent: 23.8,
        hash: "mock-study-tools",
        cache_role: "tools",
      },
    ],
    prefix_units: [
      { hash: "mock-study-system", tokens: 7_200 },
      { hash: "mock-study-tools", tokens: 9_800 },
    ],
  },
};

export function mockStudyNodeDetail(
  graph: StudyGraphSnapshot,
  nodeId: string,
): StudyNodeDetail | null {
  const summary = graph.nodes.find((node) => node.id === nodeId);
  if (!summary) {
    return null;
  }

  const node: StudyNode = {
    id: summary.id,
    module_id: summary.module_id,
    title: summary.title,
    summary: summary.summary,
    body: mockStudyNodeBody(summary),
    aliases: [...summary.aliases],
    tags: [...summary.tags],
    sources: mockStudySources(summary),
    content_version: summary.content_version,
    created_at_ms: MOCK_STUDY_NOW_MS - 45 * 24 * 60 * 60 * 1000,
    updated_at_ms: MOCK_STUDY_NOW_MS - 2 * 24 * 60 * 60 * 1000,
  };

  return {
    node,
    progress: summary.progress,
    questions: mockStudyQuestions(summary),
    neighbors: mockStudyNeighbors(graph, summary.id),
  };
}

function mockStudyNodeBody(node: StudyNodeSummary): string {
  return [
    `## ${node.title}`,
    "",
    node.summary,
    "",
    "### Core ideas",
    "",
    `- **Prerequisites.** ${node.title} only makes sense once the incoming prerequisite nodes are comfortable.`,
    "- **Worked example.** Redo one example without looking, then compare each step with the source.",
    "- **Self-explanation.** Restate the rule in your own words and give one counterexample.",
    "",
    "### Practice plan",
    "",
    "1. Re-derive the main rule from the prerequisite definitions.",
    "2. Solve two mixed problems where the rule is not signposted.",
    "3. Explain the failure mode of the most common mistake.",
  ].join("\n");
}

function mockStudySources(node: StudyNodeSummary): StudySource[] {
  return [
    {
      title: `${node.title} overview`,
      url: `https://example.org/study/${node.id}`,
    },
    {
      title: `${node.title} practice set`,
      url: `https://example.org/study/${node.id}/practice`,
    },
  ];
}

function mockStudyQuestions(node: StudyNodeSummary): StudyQuestion[] {
  const firstOutcome =
    node.progress.understanding >= 90
      ? "correct"
      : node.progress.understanding >= 60
        ? "partially_correct"
        : null;

  return [
    {
      id: `question-${node.id}-1`,
      node_id: node.id,
      question: `State the main rule behind ${node.title} and when it does not apply.`,
      answer:
        "Name the rule, list its precondition, and give one case where the precondition fails.",
      difficulty: "easy",
      node_content_version: node.content_version,
      is_stale: false,
      created_at_ms: MOCK_STUDY_NOW_MS - 20 * 24 * 60 * 60 * 1000,
      last_outcome: firstOutcome,
    },
    {
      id: `question-${node.id}-2`,
      node_id: node.id,
      question: `Apply ${node.title} to a problem that also needs an earlier prerequisite.`,
      answer:
        "Reduce the problem to the prerequisite form first, then apply the current rule step by step.",
      difficulty: "medium",
      node_content_version: Math.max(1, node.content_version - 1),
      is_stale: node.stale_question_count > 0,
      created_at_ms: MOCK_STUDY_NOW_MS - 10 * 24 * 60 * 60 * 1000,
      last_outcome: null,
    },
  ];
}

function mockStudyNeighbors(
  graph: StudyGraphSnapshot,
  nodeId: string,
): StudyNeighbor[] {
  const neighbors: StudyNeighbor[] = [];

  for (const edge of graph.edges) {
    if (edge.to === nodeId) {
      const source = graph.nodes.find((node) => node.id === edge.from);
      if (source) {
        neighbors.push({
          node: source,
          relation: edge.relation,
          direction: "in",
          note: edge.note,
        });
      }
    } else if (edge.from === nodeId) {
      const target = graph.nodes.find((node) => node.id === edge.to);
      if (target) {
        neighbors.push({
          node: target,
          relation: edge.relation,
          direction: "out",
          note: edge.note,
        });
      }
    }
  }

  return neighbors;
}
