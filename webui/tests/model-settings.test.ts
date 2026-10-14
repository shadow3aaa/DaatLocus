import { describe, expect, test } from "bun:test";
import { reasoningValues, selectDiscoveredModel } from "../src/lib/model-settings";

describe("model directory selections", () => {
  test("a newly selected ID gets its own capacities and resets prior reasoning without losing request preferences", () => {
    const model = selectDiscoveredModel({name: "old", model_id: "old", provider_name: "gateway", supports_vision: false, thinking_budget: "high", context_window_tokens: 10, request_timeout_secs: 75}, {id: "new", context_window_tokens: 128000, max_completion_tokens: 8192, supports_vision: true, thinking_budgets: ["low", "high"]});
    expect(model).toMatchObject({name: "new", model_id: "new", provider_name: "gateway", supports_vision: true, context_window_tokens: 128000, max_completion_tokens: 8192, thinking_budget: null, request_timeout_secs: 75});
    expect(selectDiscoveredModel({...model, name: "personal alias"}, {id: "other", thinking_budgets: []}).name).toBe("personal alias");
  });
  test("token-budget suggestions respect directory limits and keep toggle/effort options", () => {
    expect(reasoningValues({id: "reasoner", thinking_budgets: ["high"], reasoning_options: [{type: "effort", values: ["medium", "high"]}, {type: "budget_tokens", min: 2000, max: 10000}, {type: "toggle"}]})).toEqual(["high", "medium", "2000", "4096", "8192", "10000", "true", "false"]);
  });
});
