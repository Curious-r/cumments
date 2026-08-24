# OpenAPI 3.2 status

Cumments' contract now targets **OpenAPI 3.2.0**. The three read operations
that use HTTP `QUERY` are modeled with the native `query` Path Item field, not
a vendor extension.

## Contract features

| Feature | Status | Notes |
|---|---|---|
| Native HTTP `QUERY` | Used | Comment list, operator site list and operator quarantine list. |
| JSON Schema nullable types | Used | OAS 3.0 `nullable` has been replaced by JSON Schema 2020-12 forms. |
| `$self` | Used | Declares the canonical contract URL. |
| SSE `itemSchema` | Used | Describes each parsed Server-Sent Events frame. |
| JSON-valued SSE data | Used | Modeled with `contentMediaType: application/json` and `contentSchema`. |
| Tag hierarchy | Not used | Flat tags remain stable until renderer support matures. |
| `additionalOperations` | Not used | No non-standard HTTP methods are exposed. |
| `in: querystring` | Not used | Explicit query parameters remain more discoverable and validatable. |

## Tool compatibility

Last reviewed: 2026-08-25.

| Tool / library | Version tested or reviewed | Result |
|---|---|---|
| Redocly CLI | 2.47.0 | Lint and bundle pass for the Cumments contract, including native `query` and SSE `itemSchema`. |
| Redocly Redoc | 2.5.3 reviewed | Renderer support for all OpenAPI 3.2 features is still maturing; upstream issues remain open. We publish the raw contract and do not depend on Redoc rendering every new keyword. |
| Swagger UI | 5.32.14 reviewed | Upstream has partial/basic OpenAPI 3.2 work; additional-operation support is still in progress. Treat interactive rendering as best-effort. |
| Scalar | active release line reviewed | Active development; verify a specific deployment before relying on interactive QUERY support. |
| OpenAPI Generator | 7.25.0 reviewed | No complete OpenAPI 3.2 support guarantee is claimed by the migration. Revalidate a target generator before adopting client generation. |

CI pins the Redocly CLI version and runs structural assertions so accidental
downgrade or reintroduction of private QUERY extensions fails before merge.
