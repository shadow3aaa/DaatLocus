Publish files, images, SVG, HTML, or URLs as inline results. Exactly one of
resource, path, content, and url is required; set the other sources to null.

Resources are shared infrastructure for all Apps. Tool observations may return
resource_references containing an opaque id and an internal uri. Pass either to
present.resource to deliver an existing image/SVG/HTML resource directly. An
internal uri can also be used in Markdown links and image embeds, or passed to
actors__read_resource to read the original bytes. Resource identity and access
remain scoped to this Actor; a reference is not a filesystem grant or permission
to read another Actor's data. Do not search internal caches or construct HTTP hosts.
For an image already returned by a tool, `![caption](internal-uri)` displays it in
your prose; present.resource creates an explicit delivered result with a title.

Use path for an absolute or workspace-relative local image/SVG/HTML file. Normal
file permissions and any required additional read approval still apply. Publication
stores a snapshot; later file changes do not change the delivered image. Use
content for inline SVG/HTML, and url for an external http/https address. Reuse the
returned artifact_id when publishing a new version.
