import { describe, expect, test } from "bun:test";
import { removeConfiguredModel, removeProvider, reasoningValues, selectDiscoveredModel } from "../src/lib/model-settings";
import type { SetupConfigRequest } from "../src/lib/actor-api";

const configuration: SetupConfigRequest = {providers:[{kind:"openai",name:"old"},{kind:"openai",name:"remaining"}],models:[
  {name:"old-model",provider_name:"old",model_id:"old"},
  {name:"second",provider_name:"remaining",model_id:"second"},
  {name:"third",provider_name:"remaining",model_id:"third"},
],main_model:"old-model",approval_model:"old-model"};
test("removing a previously configured provider cascades its models and falls back in list order",()=>{
  const next=removeProvider(configuration,0);
  expect(next.providers.map(provider=>provider.name)).toEqual(["remaining"]);
  expect(next.models.map(model=>model.name)).toEqual(["second","third"]);
  expect(next.main_model).toBe("second");expect(next.approval_model).toBeNull();
  const unused=removeProvider({...configuration,main_model:"third",approval_model:"second"},0);
  expect(unused.main_model).toBe("third");expect(unused.approval_model).toBe("second");
  expect(configuration.models).toHaveLength(3);
});
test("deleting the last provider or model produces a savable unconfigured selection",()=>{
  const next=removeProvider(removeProvider(configuration,0),0);
  expect(next.providers).toEqual([]);expect(next.models).toEqual([]);expect(next.main_model).toBeNull();expect(next.approval_model).toBeNull();
  const model=removeConfiguredModel({...configuration,models:configuration.models.slice(0,1)},0);
  expect(model.main_model).toBeNull();expect(model.approval_model).toBeNull();
});

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
