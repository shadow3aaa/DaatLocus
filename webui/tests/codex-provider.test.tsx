import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ProviderEditor } from "../src/components/settings/provider-editor";
import { changeProviderKind } from "../src/lib/model-settings";

test("Codex custom API authentication exposes a required endpoint and password without login actions", () => {
  const provider = changeProviderKind({kind:"openai",name:"gateway"}, "codex");
  expect(provider.codex_auth_method).toBe("api_key");
  const html = renderToStaticMarkup(<ProviderEditor provider={provider} onChange={()=>{}} />);
  expect(html).toContain('id="dl-provider-key"');
  expect(html).toContain('type="password"');
  expect(html).toContain('required=""');
  expect(html).not.toContain('id="dl-provider-auth-file"');
  expect(html).not.toContain("Read auth file");
  const login = renderToStaticMarkup(<ProviderEditor provider={{kind:"codex",name:"login",codex_auth_method:"existing_auth_file",codex_auth_file:"private.json"}} onChange={()=>{}} />);
  expect(login).toContain('id="dl-provider-auth-file"');
  expect(login).not.toContain('id="dl-provider-key"');
});
