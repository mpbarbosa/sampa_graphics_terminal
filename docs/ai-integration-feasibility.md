# Feasibility — integrating the shell with the Claude API

> **Status: implemented** (the plan in §5 was approved and built). See
> `crates/ai` (`sampa-ai`), the `suggest_command` bridge command, the `Ctrl+Shift+A`
> overlay in `src/main.ts`, and the `[ai]` config block. This doc remains the design
> rationale for *why* it is opt-in and how the guardrails are shaped.

**Question:** can Sampa integrate with the Claude (Anthropic) API — e.g. describe a
command's output and get back the command line that produces it?

**Short answer:** Technically yes, and it fits Sampa's existing *insert-never-run* UX and
headless-core seam cleanly. But it crosses the one line the project drew on purpose —
**zero network surface** (DESIGN.md §13; "features are in-process function calls, never
ports") — and it sends terminal content to a third party. So it's feasible as a strictly
**opt-in, disabled-by-default, key-gated, consented** feature, not as a v1 default.

---

## 1. What the feature is

Two directions, both a **single `POST /v1/messages` call**:

- **NL → command** (the common case): "list files bigger than 100 MB" → `find . -size +100M`.
- **Output → command** (the asked example): paste/point at some output → the command that
  likely produced it. A reverse/"explain" variant of the same call.

In both, the result is a **suggested command line**, plus a short explanation.

## 2. How it fits the architecture

It maps onto patterns Sampa already has, so the renderer-agnostic core stays clean:

- **New headless crate `crates/ai` (`sampa-ai`)** — GUI-free, like the other service crates.
  Rust has no official Anthropic SDK, so it calls the Messages API over **raw HTTPS**
  (`reqwest`): `POST https://api.anthropic.com/v1/messages`, headers `x-api-key` +
  `anthropic-version: 2023-06-01`. Unit-testable by mocking the HTTP layer. **No Tauri
  imports** — same rule as `pty-core`/`config`/`preview`.
- **Bridge command** `suggest_command(description, context) -> { command, explanation }`,
  following the existing command/event split in `src-tauri/src/lib.rs`.
- **Structured output** — send `output_config.format` with a `json_schema` for
  `{ command: string, explanation: string }` so parsing is guaranteed (supported on
  `claude-opus-5`, `claude-sonnet-5`, `claude-haiku-4-5`). `max_tokens` ~1024; no assistant
  prefill (removed on current models).
- **Frontend UX = the palette pattern.** An overlay (like `Ctrl+Shift+P`) takes a
  natural-language prompt; the returned command is **inserted at the prompt, never
  auto-run** — exactly the §10.1 / §13 boundary the command palette already honors. If the
  user then hits preview (`Ctrl+Shift+R`), the existing `sampa_preview::classify` gate still
  applies. The service stays in the core; only the overlay lives in `src/main.ts`.
- **Config** — a new `[ai]` block, **off by default**: `enabled = false`, `model`
  (default `claude-opus-5`; `claude-haiku-4-5`/`claude-sonnet-5` for lower latency/cost),
  `endpoint` (override for a proxy or a **local** model), and a `context` scope knob (§4).

## 3. The real issue — it breaks "zero network surface"

Everything above is straightforward. The weight of this decision is the security/privacy
posture, because Sampa deliberately has **no outbound network today**:

1. **Network egress.** This is the first feature that opens a socket to the outside world.
   It must be **opt-in** (`[ai] enabled` default `false`) and every request needs the same
   treatment as any "send content to an external service" action — a **confirmation**,
   because sending publishes.
2. **The API key is a secret.** Never in the repo, never in `config.toml`. Read it from the
   environment (`ANTHROPIC_API_KEY`); the app never stores or prompts for it in plaintext.
   (Handling credentials in-app is exactly the class of action Sampa should push back to the
   user.)
3. **Terminal content is sensitive.** The description — and any context you attach (recent
   output, cwd, shell) — can contain tokens, keys, PII, hostnames. Default to sending the
   **least** possible: the user's typed request only. Attaching scrollback/output should be
   explicit (a keybinding or a selection), never automatic, and worth a redaction pass for
   obvious secret shapes before egress. This is the inverse of the preview gate: preview
   keeps data *local*; this feature is the one place data leaves.
4. **It stays advisory.** Reuse the insert-never-run rule: the model's output is a
   suggestion placed at the prompt. The security boundary that a typed `rm` is
   filesystem-verified-never-run (§13) is unchanged — the AI never executes anything.

## 4. Trade-offs

**Gains**
- A genuinely useful NL→command / explain-output capability, riding UX Sampa already has.
- Clean architectural fit — a headless service behind the existing seam; no core rewrite.

**Costs / risks**
- Opens the project's first network surface; adds an API-key dependency and a per-request
  cost + latency (a network round-trip vs. the palette's instant local filter).
- Sends terminal content to a third party — the privacy surface has to be designed, not
  bolted on.
- Degrades offline; adds an external dependency to a tool whose whole point is the local
  shell.

**Privacy-preserving alternative worth calling out:** because the Messages API shape is
simple and the `endpoint` is configurable, the *same* `crates/ai` can point at a **local
model** (e.g. an Ollama/OpenAI-compatible endpoint, or a self-hosted proxy). That keeps the
"nothing leaves the machine" posture while still delivering NL→command — a strong default
for privacy-sensitive users, with the hosted Claude API as an opt-in upgrade.

## 5. Recommendation

Feasible and a natural feature — **but ship it opt-in, not as a v1 default.** Concretely:

1. A headless `crates/ai` service (raw HTTPS to `POST /v1/messages`, structured
   `{command, explanation}` output, model + endpoint from config), unit-tested with a mocked
   transport. No GUI imports.
2. Off by default (`[ai] enabled = false`); API key from the environment, never stored.
3. Minimal, **consented** context — typed request only unless the user explicitly attaches
   output; confirm before each send.
4. Result **inserted at the prompt, never auto-run** (reuse the §10.1/§13 boundary); the
   preview gate still governs any subsequent run.
5. Make `endpoint` configurable so a **local model** preserves the zero-third-party-egress
   posture for users who want the feature without the network trade-off.

Treat it as an M6/post-v1 optional capability. It is the first feature that trades Sampa's
zero-network-surface guarantee for functionality, so the guardrails above are the feature —
the API call itself is the easy part.
