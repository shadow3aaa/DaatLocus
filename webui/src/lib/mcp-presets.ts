// Official hosted Streamable HTTP endpoints. Presets only prefill the generic form.
// https://github.com/github/github-mcp-server/blob/main/docs/remote-server.md
// https://github.com/MicrosoftDocs/mcp
// https://github.com/upstash/context7
export const mcpPresets = [
  { id: "github", name: "GitHub", url: "https://api.githubcopilot.com/mcp/", auth: "token" },
  { id: "microsoft-learn", name: "Microsoft Learn", url: "https://learn.microsoft.com/api/mcp", auth: "none" },
  { id: "context7", name: "Context7", url: "https://mcp.context7.com/mcp", auth: "token" },
] as const;

export function mcpPresetDraft(id: string) {
  const preset = mcpPresets.find((item) => item.id === id);
  if (!preset) return null;
  return {
    name: preset.name,
    transport: "http" as const,
    url: preset.url,
    auth: preset.auth,
    // Switching servers must never carry credentials from the previous draft.
    token: "",
    command: "",
    argumentsText: "",
    environment: [] as { key: string; value: string }[],
  };
}
